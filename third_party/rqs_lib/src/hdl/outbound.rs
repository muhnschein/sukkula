use std::collections::HashMap;
use std::time::Duration;

use anyhow::anyhow;
use bytes::Bytes;
use hmac::{Hmac, Mac};
use p256::ecdh::diffie_hellman;
use p256::elliptic_curve::sec1::ToEncodedPoint;
use prost::Message;
use rand::Rng;
use sha2::{Digest, Sha256, Sha512};
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::net::TcpStream;

use super::frame::{FrameReader, MAX_HANDSHAKE_FRAME_LENGTH, SANE_FRAME_LENGTH, write_frame};
use super::info::{InternalFileInfo, OutgoingFile, OutgoingText};
use super::payload::{MAX_CONTROL_PAYLOAD_LENGTH, assemble};
use super::{InnerState, TextPayloadType, TransferState};
use crate::location_nearby_connections::bandwidth_upgrade_negotiation_frame::upgrade_path_info::Medium;
use crate::location_nearby_connections::connection_response_frame::ResponseStatus;
use crate::location_nearby_connections::payload_transfer_frame::{
    PacketType, PayloadChunk, PayloadHeader, payload_header,
};
use crate::location_nearby_connections::{KeepAliveFrame, OfflineFrame, PayloadTransferFrame};
use crate::securegcm::ukey2_alert::AlertType;
use crate::securegcm::ukey2_client_init::CipherCommitment;
use crate::securegcm::{
    DeviceToDeviceMessage, GcmMetadata, Type, Ukey2Alert, Ukey2ClientFinished, Ukey2ClientInit,
    Ukey2HandshakeCipher, Ukey2Message, Ukey2ServerInit, ukey2_message,
};
use crate::securemessage::{
    EcP256PublicKey, EncScheme, GenericPublicKey, Header, HeaderAndBody, PublicKeyType,
    SecureMessage, SigScheme,
};
use crate::sharing_nearby::{
    FileMetadata, IntroductionFrame, TextMetadata, file_metadata, paired_key_result_frame,
    text_metadata,
};
use crate::utils::{
    DeviceType, RemoteDeviceInfo, aes_cbc_decrypt, aes_cbc_encrypt, decode_p256_point,
    encode_point, gen_ecdsa_keypair, gen_random, hkdf_extract_expand, to_four_digit_string,
};
use crate::{location_nearby_connections, sharing_nearby};

type HmacSha256 = Hmac<Sha256>;

/// A single chunk write that blocks this long means the peer stopped reading;
/// abort cleanly instead of hanging the transfer (and the UI).
const CHUNK_WRITE_TIMEOUT: Duration = Duration::from_secs(20);

/// What is offered to the receiver.
#[derive(Debug, Clone)]
pub enum OutboundPayload {
    /// Files; the application hands their bytes over with
    /// [`OutboundRequest::send_file_chunk`].
    Files(Vec<OutgoingFile>),
    /// One text, sent whole with [`OutboundRequest::send_text`].
    Text(OutgoingText),
}

/// What a sending connection has for the embedding application.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OutboundEvent {
    /// The introduction went out; the receiver is asking its user.
    IntroductionSent,
    /// The receiver accepted: send the payload now.
    Accepted,
    /// The receiver declined, ran out of space, or its prompt timed out.
    Rejected(sharing_nearby::connection_response_frame::Status),
    /// The receiver cancelled.
    Cancelled,
    /// The receiver hung up.
    Disconnected,
}

/// The sending side of one connection, driven by the embedding application
/// like [`crate::hdl::InboundRequest`].
#[derive(Debug)]
pub struct OutboundRequest<S = TcpStream> {
    endpoint_id: [u8; 4],
    device_name: String,
    socket: S,
    reader: FrameReader,
    pub state: InnerState,
    payload: OutboundPayload,
    /// Payload ids of the files, in the order they were offered.
    file_ids: Vec<i64>,
    /// The payload id of the text, if one was offered.
    text_id: Option<i64>,
    /// The peer's OS as reported in its ConnectionResponse. Windows receivers
    /// get no DISCONNECTION frame at the end.
    peer_os: Option<i32>,
}

impl<S: AsyncRead + AsyncWrite + Unpin> OutboundRequest<S> {
    /// A request that will introduce itself as `device_name`.
    pub fn new(
        endpoint_id: [u8; 4],
        socket: S,
        device_name: String,
        payload: OutboundPayload,
    ) -> Self {
        Self {
            endpoint_id,
            device_name,
            socket,
            reader: FrameReader::new(),
            state: InnerState {
                server_seq: 0,
                client_seq: 0,
                state: TransferState::Initial,
                // No keys until the UKEY2 exchange is done: until then a
                // keep-alive or goodbye goes out in plaintext rather than
                // reaching an unwrap of a key that is not there.
                encryption_done: false,
                ..Default::default()
            },
            payload,
            file_ids: Vec::new(),
            text_id: None,
            peer_os: None,
        }
    }

    /// The PIN both screens show, once the key exchange is done.
    pub fn pin_code(&self) -> Option<&str> {
        self.state.pin_code.as_deref()
    }

    /// The payload id of each offered file, in order, once introduced.
    pub fn file_ids(&self) -> &[i64] {
        &self.file_ids
    }

    /// Whether the next frame from the receiver is still a plaintext
    /// handshake frame.
    fn is_handshaking(&self) -> bool {
        matches!(
            self.state.state,
            TransferState::Initial
                | TransferState::SentUkeyClientInit
                | TransferState::SentUkeyClientFinish
        )
    }

    /// Reads the next frame. Cancel-safe; see [`FrameReader`].
    pub async fn read_frame(&mut self) -> Result<Vec<u8>, anyhow::Error> {
        let max = if self.is_handshaking() {
            MAX_HANDSHAKE_FRAME_LENGTH
        } else {
            SANE_FRAME_LENGTH
        };
        self.reader.read_frame(&mut self.socket, max).await
    }

    /// Reads and processes the next frame. Not cancel-safe.
    pub async fn next_event(&mut self) -> Result<Option<OutboundEvent>, anyhow::Error> {
        let frame = self.read_frame().await?;
        self.process_frame(frame).await
    }

    /// Processes one frame from [`read_frame`](Self::read_frame). Not
    /// cancel-safe.
    pub async fn process_frame(
        &mut self,
        frame_data: Vec<u8>,
    ) -> Result<Option<OutboundEvent>, anyhow::Error> {
        if frame_data.len() > SANE_FRAME_LENGTH {
            return Err(anyhow!("Message length too big"));
        }

        let current_state = &self.state;
        // Now determine what will be the request type based on current state
        match current_state.state {
            TransferState::SentUkeyClientInit => {
                debug!("Handling State::SentUkeyClientInit frame");
                let msg = Ukey2Message::decode(&*frame_data)?;
                self.update_state(|e| {
                    e.server_init_data = Some(frame_data);
                });
                self.process_ukey2_server_init(&msg).await?;

                // Advance current state
                self.update_state(|e: &mut InnerState| {
                    e.state = TransferState::SentUkeyClientFinish;
                    e.encryption_done = true;
                });
            }
            TransferState::SentUkeyClientFinish => {
                debug!("Handling State::SentUkeyClientFinish frame");
                let frame = location_nearby_connections::OfflineFrame::decode(&*frame_data)?;
                self.process_connection_response(&frame).await?;

                // Advance current state
                self.update_state(|e: &mut InnerState| {
                    e.state = TransferState::SentPairedKeyEncryption;
                    e.server_init_data = Some(frame_data);
                    e.encryption_done = true;
                });
            }
            _ => {
                debug!("Handling SecureMessage frame");
                let smsg = SecureMessage::decode(&*frame_data)?;
                return self.decrypt_and_process_secure_message(&smsg).await;
            }
        }

        Ok(None)
    }

    pub async fn send_connection_request(&mut self) -> Result<(), anyhow::Error> {
        let device_name = self.device_name.clone();
        let request = location_nearby_connections::OfflineFrame {
            version: Some(location_nearby_connections::offline_frame::Version::V1.into()),
            v1: Some(location_nearby_connections::V1Frame {
                r#type: Some(
                    location_nearby_connections::v1_frame::FrameType::ConnectionRequest.into(),
                ),
                connection_request: Some(location_nearby_connections::ConnectionRequestFrame {
                    endpoint_id: Some(String::from_utf8_lossy(&self.endpoint_id).to_string()),
                    endpoint_name: Some(device_name.clone().into()),
                    endpoint_info: Some(
                        RemoteDeviceInfo {
                            name: device_name,
                            device_type: DeviceType::Phone,
                        }
                        .serialize(),
                    ),
                    // Wi-Fi LAN only: no upgrade medium is offered, so the
                    // peer never asks us to join or host another network.
                    mediums: vec![Medium::WifiLan.into()],
                    ..Default::default()
                }),
                ..Default::default()
            }),
        };

        self.send_frame(request.encode_to_vec()).await?;

        Ok(())
    }

    pub async fn send_ukey2_client_init(&mut self) -> Result<(), anyhow::Error> {
        let (secret_key, public_key) = gen_ecdsa_keypair();

        let encoded_point = public_key.to_encoded_point(false);
        let (Some(x), Some(y)) = (encoded_point.x(), encoded_point.y()) else {
            return Err(anyhow!("own key has no coordinates"));
        };

        let pkey = GenericPublicKey {
            r#type: PublicKeyType::EcP256.into(),
            ec_p256_public_key: Some(EcP256PublicKey {
                x: encode_point(Bytes::from(x.to_vec()))?,
                y: encode_point(Bytes::from(y.to_vec()))?,
            }),
            ..Default::default()
        };

        let finish_frame = Ukey2Message {
            message_type: Some(ukey2_message::Type::ClientFinish.into()),
            message_data: Some(
                Ukey2ClientFinished {
                    public_key: Some(pkey.encode_to_vec()),
                }
                .encode_to_vec(),
            ),
        };

        let sha512 = Sha512::digest(finish_frame.encode_to_vec());
        let frame = Ukey2Message {
            message_type: Some(ukey2_message::Type::ClientInit.into()),
            message_data: Some(
                Ukey2ClientInit {
                    version: Some(1),
                    random: Some(gen_random(32)),
                    next_protocol: Some(String::from("AES_256_CBC-HMAC_SHA256")),
                    cipher_commitments: vec![CipherCommitment {
                        handshake_cipher: Some(Ukey2HandshakeCipher::P256Sha512.into()),
                        commitment: Some(sha512.to_vec()),
                    }],
                }
                .encode_to_vec(),
            ),
        };

        self.send_frame(frame.encode_to_vec()).await?;

        self.update_state(|e| {
            e.state = TransferState::SentUkeyClientInit;
            e.private_key = Some(secret_key);
            e.public_key = Some(public_key);
            e.client_init_msg_data = Some(frame.encode_to_vec());
            e.ukey_client_finish_msg_data = Some(finish_frame.encode_to_vec());
        });

        Ok(())
    }

    async fn process_ukey2_server_init(&mut self, msg: &Ukey2Message) -> Result<(), anyhow::Error> {
        if msg.message_type() != ukey2_message::Type::ServerInit {
            self.send_ukey2_alert(AlertType::BadMessageType).await?;
            return Err(anyhow!(
                "UKey2: message_type({:?}) != ServerInit",
                msg.message_type
            ));
        }

        let server_init = match Ukey2ServerInit::decode(msg.message_data()) {
            Ok(uk2si) => uk2si,
            Err(e) => {
                return Err(anyhow!("UKey2: Ukey2ClientFinished::decode: {}", e));
            }
        };

        if server_init.version() != 1 {
            self.send_ukey2_alert(AlertType::BadVersion).await?;
            return Err(anyhow!("UKey2: server_init.version != 1"));
        }

        if server_init.random().len() != 32 {
            self.send_ukey2_alert(AlertType::BadRandom).await?;
            return Err(anyhow!("UKey2: server_init.random.len != 32"));
        }

        if server_init.handshake_cipher() != Ukey2HandshakeCipher::P256Sha512 {
            self.send_ukey2_alert(AlertType::BadHandshakeCipher).await?;
            return Err(anyhow!("UKey2: handshake_cipher != P256Sha512"));
        }

        let server_public_key = match GenericPublicKey::decode(server_init.public_key()) {
            Ok(spk) => spk,
            Err(e) => {
                return Err(anyhow!("UKey2: GenericPublicKey::decode: {}", e));
            }
        };

        self.finalize_key_exchange(server_public_key).await?;
        let client_finish = self
            .state
            .ukey_client_finish_msg_data
            .clone()
            .ok_or_else(|| anyhow!("UKey2: no ClientFinish prepared"))?;
        self.send_frame(client_finish).await?;

        let frame = location_nearby_connections::OfflineFrame {
            version: Some(location_nearby_connections::offline_frame::Version::V1.into()),
            v1: Some(location_nearby_connections::V1Frame {
                r#type: Some(
                    location_nearby_connections::v1_frame::FrameType::ConnectionResponse.into(),
                ),
                connection_response: Some(location_nearby_connections::ConnectionResponseFrame {
                    response: Some(
                        location_nearby_connections::connection_response_frame::ResponseStatus::Accept.into(),
                    ),
                    // Linux, as the receive path says too. Presenting as Windows
                    // only served to trigger the peer's Wi-Fi Direct role
                    // switch, which this build does not do.
                    os_info: Some(location_nearby_connections::OsInfo {
                        r#type: Some(location_nearby_connections::os_info::OsType::Linux.into()),
                    }),
                    ..Default::default()
                }),
                ..Default::default()
            }),
        };

        self.send_frame(frame.encode_to_vec()).await?;

        Ok(())
    }

    async fn process_connection_response(
        &mut self,
        frame: &location_nearby_connections::OfflineFrame,
    ) -> Result<(), anyhow::Error> {
        let v1_frame = frame
            .v1
            .as_ref()
            .ok_or_else(|| anyhow!("Missing required fields"))?;

        if v1_frame.r#type() != location_nearby_connections::v1_frame::FrameType::ConnectionResponse
        {
            return Err(anyhow!(format!(
                "Unexpected frame type: {:?}",
                v1_frame.r#type()
            )));
        }

        let Some(connection_response) = v1_frame.connection_response.as_ref() else {
            return Err(anyhow!(format!("Unexpected None connection_response",)));
        };

        // Remember who we're talking to: Windows receivers get a quiet close.
        self.peer_os = connection_response.os_info.as_ref().and_then(|o| o.r#type);
        info!("peer os_info: {:?}", self.peer_os);

        if connection_response.response() != ResponseStatus::Accept {
            return Err(anyhow!(format!("Connection rejected by third party",)));
        }

        let paired_encryption = sharing_nearby::Frame {
            version: Some(sharing_nearby::frame::Version::V1.into()),
            v1: Some(sharing_nearby::V1Frame {
                r#type: Some(sharing_nearby::v1_frame::FrameType::PairedKeyEncryption.into()),
                paired_key_encryption: Some(sharing_nearby::PairedKeyEncryptionFrame {
                    secret_id_hash: Some(gen_random(6)),
                    signed_data: Some(gen_random(72)),
                    ..Default::default()
                }),
                ..Default::default()
            }),
        };

        self.send_encrypted_frame(&paired_encryption).await?;

        Ok(())
    }

    async fn decrypt_and_process_secure_message(
        &mut self,
        smsg: &SecureMessage,
    ) -> Result<Option<OutboundEvent>, anyhow::Error> {
        let offline = self.decrypt_secure_message(smsg)?;
        let v1_frame = offline
            .v1
            .as_ref()
            .ok_or_else(|| anyhow!("Missing required fields"))?;
        debug!(
            "outbound recv offline frame: type={:?} (state={:?})",
            v1_frame.r#type(),
            self.state.state
        );
        match v1_frame.r#type() {
            location_nearby_connections::v1_frame::FrameType::PayloadTransfer => {
                trace!("Received FrameType::PayloadTransfer");
                let payload_transfer = v1_frame
                    .payload_transfer
                    .as_ref()
                    .ok_or_else(|| anyhow!("Missing required fields"))?;

                let header = payload_transfer
                    .payload_header
                    .as_ref()
                    .ok_or_else(|| anyhow!("Missing required fields"))?;
                let chunk = payload_transfer
                    .payload_chunk
                    .as_ref()
                    .ok_or_else(|| anyhow!("Missing required fields"))?;

                match header.r#type() {
                    payload_header::PayloadType::Bytes => {
                        trace!("Processing PayloadType::Bytes");
                        let payload_id = header.id();

                        let assembled = assemble(
                            &mut self.state.payload_buffers,
                            payload_id,
                            header.total_size(),
                            MAX_CONTROL_PAYLOAD_LENGTH,
                            chunk,
                        )?;

                        if let Some(buffer) = assembled {
                            debug!("Chunk flags & 1 == 1 ?? End of data ??");

                            let inner_frame = sharing_nearby::Frame::decode(buffer.as_slice())?;
                            return self.process_transfer_setup(&inner_frame).await;
                        }
                    }
                    payload_header::PayloadType::File => {
                        error!("Unhandled PayloadType::File: {:?}", header.r#type())
                    }
                    payload_header::PayloadType::Stream => {
                        error!("Unhandled PayloadType::Stream: {:?}", header.r#type())
                    }
                    payload_header::PayloadType::UnknownPayloadType => {
                        error!(
                            "Invalid PayloadType::UnknownPayloadType: {:?}",
                            header.r#type()
                        )
                    }
                }
            }
            location_nearby_connections::v1_frame::FrameType::KeepAlive => {
                trace!("Sending keepalive");
                self.send_keepalive(true).await?;
            }
            location_nearby_connections::v1_frame::FrameType::Disconnection => {
                self.update_state(|e| {
                    e.state = TransferState::Disconnected;
                });
                return Ok(Some(OutboundEvent::Disconnected));
            }
            _ => {
                // The frame's type only: its fields are the peer's data.
                debug!("Unhandled offline frame type {:?}", v1_frame.r#type());
            }
        }

        Ok(None)
    }

    async fn process_transfer_setup(
        &mut self,
        frame: &sharing_nearby::Frame,
    ) -> Result<Option<OutboundEvent>, anyhow::Error> {
        let v1_frame = frame
            .v1
            .as_ref()
            .ok_or_else(|| anyhow!("Missing required fields"))?;
        debug!(
            "outbound recv sharing frame: type={:?} (state={:?})",
            v1_frame.r#type(),
            self.state.state
        );

        if v1_frame.r#type() == sharing_nearby::v1_frame::FrameType::Cancel {
            info!("Transfer canceled");
            self.update_state(|e| {
                e.state = TransferState::Cancelled;
            });
            self.disconnection().await?;
            return Ok(Some(OutboundEvent::Cancelled));
        }

        match self.state.state {
            TransferState::SentPairedKeyEncryption => {
                debug!("Processing State::SentPairedKeyEncryption");
                self.process_paired_key_encryption_frame(v1_frame).await?;
                self.update_state(|e| {
                    e.state = TransferState::SentPairedKeyResult;
                });
            }
            TransferState::SentPairedKeyResult => {
                debug!("Processing State::SentPairedKeyResult");
                self.process_paired_key_result(v1_frame).await?;
                self.update_state(|e| {
                    e.state = TransferState::SentIntroduction;
                });
                return Ok(Some(OutboundEvent::IntroductionSent));
            }
            TransferState::SentIntroduction => {
                debug!("Processing State::SentIntroduction");
                return self.process_consent(v1_frame).await.map(Some);
            }
            TransferState::SendingFiles => {}
            _ => {
                info!(
                    "Unhandled connection state in process_transfer_setup: {:?}",
                    self.state.state
                );
            }
        }

        Ok(None)
    }

    async fn process_paired_key_encryption_frame(
        &mut self,
        v1_frame: &sharing_nearby::V1Frame,
    ) -> Result<(), anyhow::Error> {
        if v1_frame.paired_key_encryption.is_none() {
            return Err(anyhow!("Missing required fields"));
        }

        let paired_result = sharing_nearby::Frame {
            version: Some(sharing_nearby::frame::Version::V1.into()),
            v1: Some(sharing_nearby::V1Frame {
                r#type: Some(sharing_nearby::v1_frame::FrameType::PairedKeyResult.into()),
                paired_key_result: Some(sharing_nearby::PairedKeyResultFrame {
                    status: Some(paired_key_result_frame::Status::Unable.into()),
                }),
                ..Default::default()
            }),
        };

        self.send_encrypted_frame(&paired_result).await?;

        Ok(())
    }

    /// Positive like Google's own implementations generate -- a strict
    /// receiver (Windows) may discard payloads with a negative id as invalid.
    fn new_id() -> i64 {
        rand::rng().random::<i64>().unsigned_abs() as i64 & i64::MAX
    }

    async fn process_paired_key_result(
        &mut self,
        v1_frame: &sharing_nearby::V1Frame,
    ) -> Result<(), anyhow::Error> {
        if v1_frame.paired_key_result.is_none() {
            return Err(anyhow!("Missing required fields"));
        }

        let mut file_metadata: Vec<FileMetadata> = vec![];
        let mut text_metadata_list: Vec<TextMetadata> = vec![];
        let mut transferred_files: HashMap<i64, InternalFileInfo> = HashMap::new();
        let mut file_ids = Vec::new();
        let mut text_id = None;
        match &self.payload {
            OutboundPayload::Files(files) => {
                for f in files {
                    let meta_type = if f.mime_type.starts_with("image/") {
                        file_metadata::Type::Image
                    } else if f.mime_type.starts_with("video/") {
                        file_metadata::Type::Video
                    } else if f.mime_type.starts_with("audio/") {
                        file_metadata::Type::Audio
                    } else if f.name.ends_with(".apk") {
                        file_metadata::Type::App
                    } else {
                        file_metadata::Type::Unknown
                    };

                    let fmeta = FileMetadata {
                        payload_id: Some(Self::new_id()),
                        name: Some(f.name.clone()),
                        size: Some(f.size),
                        mime_type: Some(f.mime_type.clone()),
                        r#type: Some(meta_type.into()),
                        // The attachment uuid ("Should be unique across all
                        // attachments"). Receivers key their transfer
                        // bookkeeping on it — Android tolerates its absence,
                        // Windows sits at "Connecting…" without it while the
                        // payload still saves.
                        id: Some(Self::new_id()),
                        ..Default::default()
                    };
                    transferred_files.insert(
                        fmeta.payload_id(),
                        InternalFileInfo {
                            payload_id: fmeta.payload_id(),
                            bytes_transferred: 0,
                            total_size: fmeta.size(),
                            chunks: 0,
                        },
                    );
                    file_ids.push(fmeta.payload_id());
                    file_metadata.push(fmeta);
                }
            }
            OutboundPayload::Text(text) => {
                let id = Self::new_id();
                text_id = Some(id);
                text_metadata_list.push(TextMetadata {
                    text_title: Some(text.title.clone()),
                    r#type: Some(
                        match text.kind {
                            TextPayloadType::Url => text_metadata::Type::Url,
                            TextPayloadType::Text => text_metadata::Type::Text,
                        }
                        .into(),
                    ),
                    payload_id: Some(id),
                    size: Some(text.text.len() as i64),
                    id: Some(Self::new_id()),
                });
            }
        }

        self.file_ids = file_ids;
        self.text_id = text_id;
        self.update_state(|e| {
            e.transferred_files = transferred_files;
        });

        let introduction = sharing_nearby::Frame {
            version: Some(sharing_nearby::frame::Version::V1.into()),
            v1: Some(sharing_nearby::V1Frame {
                r#type: Some(sharing_nearby::v1_frame::FrameType::Introduction.into()),
                introduction: Some(IntroductionFrame {
                    file_metadata,
                    text_metadata: text_metadata_list,
                    ..Default::default()
                }),
                ..Default::default()
            }),
        };

        self.send_encrypted_frame(&introduction).await?;

        // Consent is mutual in the sharing layer: real senders (Android and
        // Windows alike) follow the INTRODUCTION with their own
        // Response(ACCEPT). Android receivers don't miss it, but Windows
        // waits for the sender's accept before leaving "Connecting…" — and
        // reports "Can't complete transfer" without it even after saving the
        // whole payload.
        let sender_accept = sharing_nearby::Frame {
            version: Some(sharing_nearby::frame::Version::V1.into()),
            v1: Some(sharing_nearby::V1Frame {
                r#type: Some(sharing_nearby::v1_frame::FrameType::Response.into()),
                connection_response: Some(sharing_nearby::ConnectionResponseFrame {
                    status: Some(sharing_nearby::connection_response_frame::Status::Accept.into()),
                }),
                ..Default::default()
            }),
        };
        self.send_encrypted_frame(&sender_accept).await?;

        Ok(())
    }

    async fn process_consent(
        &mut self,
        v1_frame: &sharing_nearby::V1Frame,
    ) -> Result<OutboundEvent, anyhow::Error> {
        let (sharing_nearby::v1_frame::FrameType::Response, Some(response)) =
            (v1_frame.r#type(), v1_frame.connection_response.as_ref())
        else {
            return Err(anyhow!("Missing required fields"));
        };

        match response.status() {
            sharing_nearby::connection_response_frame::Status::Accept => {
                info!("State is now State::SendingFiles");
                self.update_state(|e| {
                    e.state = TransferState::SendingFiles;
                });
                Ok(OutboundEvent::Accepted)
            }
            status @ (sharing_nearby::connection_response_frame::Status::Reject
            | sharing_nearby::connection_response_frame::Status::NotEnoughSpace
            | sharing_nearby::connection_response_frame::Status::UnsupportedAttachmentType
            | sharing_nearby::connection_response_frame::Status::TimedOut) => {
                // An explicit answer from the peer (declined, out of space, or
                // its accept prompt timed out — Windows expires the prompt
                // after ~60s) — surface it as Rejected, not "unexpected
                // disconnection".
                warn!("Cannot process: consent denied: {:?}", status);
                self.update_state(|e| {
                    e.state = TransferState::Rejected;
                });
                // Best effort: a receiver that said no may already be gone.
                let _ = self.disconnection().await;
                Ok(OutboundEvent::Rejected(status))
            }
            sharing_nearby::connection_response_frame::Status::Unknown => {
                error!("Unknown consent type: aborting");
                self.update_state(|e| {
                    e.state = TransferState::Disconnected;
                });
                let _ = self.disconnection().await;
                Ok(OutboundEvent::Rejected(
                    sharing_nearby::connection_response_frame::Status::Unknown,
                ))
            }
        }
    }

    /// Sends the next `body` bytes of the file with payload id `payload_id`.
    pub async fn send_file_chunk(
        &mut self,
        payload_id: i64,
        body: &[u8],
    ) -> Result<(), anyhow::Error> {
        let (offset, total_size, index) = {
            let info = self
                .state
                .transferred_files
                .get(&payload_id)
                .ok_or_else(|| anyhow!("no such file payload"))?;
            (info.bytes_transferred, info.total_size, info.chunks)
        };
        // Never send more than was announced: a receiver sizes its file,
        // and its consent, by it.
        let len = i64::try_from(body.len())?;
        let after = offset
            .checked_add(len)
            .ok_or_else(|| anyhow!("file offset overflow"))?;
        if after > total_size {
            return Err(anyhow!("chunk goes past the announced size"));
        }
        let next_index = index
            .checked_add(1)
            .ok_or_else(|| anyhow!("too many chunks"))?;
        let name = self.file_name(payload_id);

        let payload_header = PayloadHeader {
            id: Some(payload_id),
            r#type: Some(payload_header::PayloadType::File.into()),
            total_size: Some(total_size),
            is_sensitive: Some(false),
            file_name: name,
            // Present-but-empty, matching Windows' own frames.
            parent_folder: Some(String::new()),
            ..Default::default()
        };

        let wrapper = location_nearby_connections::OfflineFrame {
            version: Some(location_nearby_connections::offline_frame::Version::V1.into()),
            v1: Some(location_nearby_connections::V1Frame {
                r#type: Some(
                    location_nearby_connections::v1_frame::FrameType::PayloadTransfer.into(),
                ),
                payload_transfer: Some(PayloadTransferFrame {
                    packet_type: Some(PacketType::Data.into()),
                    payload_chunk: Some(PayloadChunk {
                        offset: Some(offset),
                        flags: Some(0),
                        body: Some(body.to_vec()),
                        // Sequential chunk index — some receivers
                        // (Windows) reassemble by index, not offset.
                        index: Some(index),
                        ..Default::default()
                    }),
                    payload_header: Some(payload_header),
                    ..Default::default()
                }),
                ..Default::default()
            }),
        };

        tokio::time::timeout(CHUNK_WRITE_TIMEOUT, self.encrypt_and_send(&wrapper))
            .await
            .map_err(|_| anyhow!("chunk write stalled (peer stopped reading)"))??;
        if let Some(info) = self.state.transferred_files.get_mut(&payload_id) {
            info.bytes_transferred = after;
            info.chunks = next_index;
        }
        Ok(())
    }

    /// Tells the receiver the file with payload id `payload_id` is complete.
    pub async fn finish_file(&mut self, payload_id: i64) -> Result<(), anyhow::Error> {
        let (total_size, index) = {
            let info = self
                .state
                .transferred_files
                .get(&payload_id)
                .ok_or_else(|| anyhow!("no such file payload"))?;
            if info.bytes_transferred != info.total_size {
                return Err(anyhow!("file ended before its announced size"));
            }
            (info.total_size, info.chunks)
        };
        let name = self.file_name(payload_id);

        let payload_header = PayloadHeader {
            id: Some(payload_id),
            r#type: Some(payload_header::PayloadType::File.into()),
            total_size: Some(total_size),
            is_sensitive: Some(false),
            file_name: name,
            parent_folder: Some(String::new()),
            ..Default::default()
        };
        let wrapper = location_nearby_connections::OfflineFrame {
            version: Some(location_nearby_connections::offline_frame::Version::V1.into()),
            v1: Some(location_nearby_connections::V1Frame {
                r#type: Some(
                    location_nearby_connections::v1_frame::FrameType::PayloadTransfer.into(),
                ),
                payload_transfer: Some(PayloadTransferFrame {
                    packet_type: Some(PacketType::Data.into()),
                    payload_chunk: Some(PayloadChunk {
                        offset: Some(total_size),
                        flags: Some(1), // lastChunk
                        body: Some(vec![]),
                        index: Some(index),
                        ..Default::default()
                    }),
                    payload_header: Some(payload_header),
                    ..Default::default()
                }),
                ..Default::default()
            }),
        };

        tokio::time::timeout(CHUNK_WRITE_TIMEOUT, self.encrypt_and_send(&wrapper))
            .await
            .map_err(|_| anyhow!("chunk write stalled (peer stopped reading)"))??;
        self.state.transferred_files.remove(&payload_id);
        Ok(())
    }

    /// Sends the offered text, whole.
    pub async fn send_text(&mut self) -> Result<(), anyhow::Error> {
        let (OutboundPayload::Text(text), Some(id)) = (&self.payload, self.text_id) else {
            return Err(anyhow!("no text was offered"));
        };
        let body = text.text.as_bytes().to_vec();
        let payload_header = PayloadHeader {
            id: Some(id),
            r#type: Some(payload_header::PayloadType::Bytes.into()),
            total_size: Some(body.len() as i64),
            is_sensitive: Some(false),
            ..Default::default()
        };
        let size = body.len() as i64;
        for (offset, flags, body) in [(0, 0, body), (size, 1, Vec::new())] {
            let wrapper = location_nearby_connections::OfflineFrame {
                version: Some(location_nearby_connections::offline_frame::Version::V1.into()),
                v1: Some(location_nearby_connections::V1Frame {
                    r#type: Some(
                        location_nearby_connections::v1_frame::FrameType::PayloadTransfer.into(),
                    ),
                    payload_transfer: Some(PayloadTransferFrame {
                        packet_type: Some(PacketType::Data.into()),
                        payload_chunk: Some(PayloadChunk {
                            offset: Some(offset),
                            flags: Some(flags),
                            body: Some(body),
                            index: Some(i32::from(flags == 1)),
                            ..Default::default()
                        }),
                        payload_header: Some(payload_header.clone()),
                        ..Default::default()
                    }),
                    ..Default::default()
                }),
            };
            tokio::time::timeout(CHUNK_WRITE_TIMEOUT, self.encrypt_and_send(&wrapper))
                .await
                .map_err(|_| anyhow!("chunk write stalled (peer stopped reading)"))??;
        }
        Ok(())
    }

    fn file_name(&self, payload_id: i64) -> Option<String> {
        let OutboundPayload::Files(files) = &self.payload else {
            return None;
        };
        let i = self.file_ids.iter().position(|id| *id == payload_id)?;
        files.get(i).map(|f| f.name.clone())
    }

    /// Ends a transfer whose payload has all been sent. The receiver hangs
    /// up first: Windows finalizes the share after the payload completes,
    /// and a DISCONNECTION frame from us in that window makes it abort with
    /// "Can't complete transfer". Android receivers close within about a
    /// second. Only if the peer holds the socket open past the grace period
    /// do we announce our own disconnection, so nobody waits on us forever.
    pub async fn finish(&mut self) -> Result<(), anyhow::Error> {
        let peer_is_windows =
            self.peer_os == Some(location_nearby_connections::os_info::OsType::Windows as i32);
        let peer_closed = self.wait_for_peer_close(Duration::from_secs(8)).await;
        if !peer_closed {
            // Windows never sends a DISCONNECTION frame as a sender and
            // treats receiving one as a broken transfer ("Can't complete")
            // even with the file already saved — mirror its etiquette:
            // linger, then hang up silently.
            if peer_is_windows {
                debug!("windows peer: closing quietly, no disconnection frame");
            } else {
                debug!("peer still connected after grace; sending disconnection");
                self.disconnection().await?;
                self.wait_for_peer_close(Duration::from_secs(2)).await;
            }
        }
        self.update_state(|e| {
            e.state = TransferState::Finished;
        });
        Ok(())
    }

    /// Cancels the transfer: a CANCEL frame, then goodbye.
    pub async fn cancel_transfer(&mut self) -> Result<(), anyhow::Error> {
        let frame = sharing_nearby::Frame {
            version: Some(sharing_nearby::frame::Version::V1.into()),
            v1: Some(sharing_nearby::V1Frame {
                r#type: Some(sharing_nearby::v1_frame::FrameType::Cancel.into()),
                ..Default::default()
            }),
        };
        self.send_encrypted_frame(&frame).await?;
        self.update_state(|e| {
            e.state = TransferState::Cancelled;
        });
        self.disconnection().await
    }

    /// Says goodbye to the receiver.
    pub async fn disconnection(&mut self) -> Result<(), anyhow::Error> {
        let frame = location_nearby_connections::OfflineFrame {
            version: Some(location_nearby_connections::offline_frame::Version::V1.into()),
            v1: Some(location_nearby_connections::V1Frame {
                r#type: Some(
                    location_nearby_connections::v1_frame::FrameType::Disconnection.into(),
                ),
                disconnection: Some(location_nearby_connections::DisconnectionFrame {
                    ..Default::default()
                }),
                ..Default::default()
            }),
        };

        if self.state.encryption_done {
            self.encrypt_and_send(&frame).await
        } else {
            self.send_frame(frame.encode_to_vec()).await
        }
    }

    /// Decrypts a SecureMessage (advancing client_seq) into its OfflineFrame.
    fn decrypt_secure_message(
        &mut self,
        smsg: &SecureMessage,
    ) -> Result<OfflineFrame, anyhow::Error> {
        let hmac_key = self
            .state
            .recv_hmac_key
            .as_ref()
            .ok_or_else(|| anyhow!("no keys yet"))?;
        let mut hmac = HmacSha256::new_from_slice(hmac_key)?;
        hmac.update(&smsg.header_and_body);
        // Constant time, unlike comparing the two byte slices.
        hmac.verify_slice(&smsg.signature)
            .map_err(|_| anyhow!("hmac!=signature"))?;

        let header_and_body = HeaderAndBody::decode(&*smsg.header_and_body)?;
        if header_and_body.header.encryption_scheme() != EncScheme::Aes256Cbc
            || header_and_body.header.signature_scheme() != SigScheme::HmacSha256
        {
            return Err(anyhow!("unexpected SecureMessage schemes"));
        }

        let msg_data = header_and_body.body;
        let key = self
            .state
            .decrypt_key
            .as_ref()
            .ok_or_else(|| anyhow!("no keys yet"))?;
        let decrypted = aes_cbc_decrypt(key, header_and_body.header.iv(), &msg_data)?;

        let d2d_msg = DeviceToDeviceMessage::decode(&*decrypted)?;

        let seq = self.get_client_seq_inc()?;
        if d2d_msg.sequence_number() != seq {
            return Err(anyhow!(
                "Error d2d_msg.sequence_number invalid ({} vs {})",
                d2d_msg.sequence_number(),
                seq
            ));
        }

        Ok(OfflineFrame::decode(d2d_msg.message())?)
    }

    /// Read one encrypted frame, decrypt it (advancing client_seq), and return
    /// the OfflineFrame without dispatching to the payload state machine.
    async fn read_encrypted_offline_frame(&mut self) -> Result<OfflineFrame, anyhow::Error> {
        let data = self.read_frame().await?;
        let smsg = SecureMessage::decode(&*data)?;
        self.decrypt_secure_message(&smsg)
    }

    /// Drain incoming frames until the peer closes the connection or `grace`
    /// elapses, ACKING ITS KEEPALIVES along the way — a receiver that pings
    /// every 5s (Windows) tears the session down if the pings go unanswered
    /// while it finalizes. Returns `true` if the peer closed (EOF or an
    /// explicit DISCONNECTION frame).
    async fn wait_for_peer_close(&mut self, grace: Duration) -> bool {
        let deadline = tokio::time::Instant::now() + grace;
        loop {
            match tokio::time::timeout_at(deadline, self.read_encrypted_offline_frame()).await {
                Err(_) => return false,    // grace elapsed, peer still connected
                Ok(Err(_)) => return true, // EOF / read error — peer is gone
                Ok(Ok(frame)) => {
                    if let Some(v1) = frame.v1.as_ref() {
                        match v1.r#type() {
                            location_nearby_connections::v1_frame::FrameType::KeepAlive => {
                                let _ = self.send_keepalive(true).await;
                            }
                            location_nearby_connections::v1_frame::FrameType::Disconnection => {
                                debug!("peer sent DISCONNECTION during close-wait");
                                return true;
                            }
                            _ => {} // drained and ignored
                        }
                    }
                }
            }
        }
    }

    async fn finalize_key_exchange(
        &mut self,
        raw_peer_key: GenericPublicKey,
    ) -> Result<(), anyhow::Error> {
        let peer_p256_key = raw_peer_key
            .ec_p256_public_key
            .ok_or_else(|| anyhow!("Missing required fields"))?;

        let peer_key = decode_p256_point(&peer_p256_key.x, &peer_p256_key.y)?;
        let priv_key = self
            .state
            .private_key
            .as_ref()
            .ok_or_else(|| anyhow!("no key pair"))?;

        let dhs = diffie_hellman(priv_key.to_nonzero_scalar(), peer_key.as_affine());
        let derived_secret = Sha256::digest(dhs.raw_secret_bytes());

        let mut ukey_info: Vec<u8> = vec![];
        let client_init = self.state.client_init_msg_data.as_ref();
        let server_init = self.state.server_init_data.as_ref();
        let (Some(client_init), Some(server_init)) = (client_init, server_init) else {
            return Err(anyhow!("UKEY2 messages missing"));
        };
        ukey_info.extend_from_slice(client_init);
        ukey_info.extend_from_slice(server_init);

        let auth_label = "UKEY2 v1 auth".as_bytes();
        let next_label = "UKEY2 v1 next".as_bytes();

        let auth_string = hkdf_extract_expand(auth_label, &derived_secret, &ukey_info, 32)?;
        let next_secret = hkdf_extract_expand(next_label, &derived_secret, &ukey_info, 32)?;

        let salt_hex = "82AA55A0D397F88346CA1CEE8D3909B95F13FA7DEB1D4AB38376B8256DA85510";
        let salt =
            hex::decode(salt_hex).map_err(|e| anyhow!("Failed to decode salt_hex: {}", e))?;

        let d2d_client = hkdf_extract_expand(&salt, &next_secret, "client".as_bytes(), 32)?;
        let d2d_server = hkdf_extract_expand(&salt, &next_secret, "server".as_bytes(), 32)?;

        let key_salt_hex = "BF9D2A53C63616D75DB0A7165B91C1EF73E537F2427405FA23610A4BE657642E";
        let key_salt = hex::decode(key_salt_hex)
            .map_err(|e| anyhow!("Failed to decode key_salt_hex: {}", e))?;

        let client_key = hkdf_extract_expand(&key_salt, &d2d_client, "ENC:2".as_bytes(), 32)?;
        let client_hmac_key = hkdf_extract_expand(&key_salt, &d2d_client, "SIG:1".as_bytes(), 32)?;
        let server_key = hkdf_extract_expand(&key_salt, &d2d_server, "ENC:2".as_bytes(), 32)?;
        let server_hmac_key = hkdf_extract_expand(&key_salt, &d2d_server, "SIG:1".as_bytes(), 32)?;

        self.update_state(|e| {
            e.decrypt_key = Some(server_key);
            e.recv_hmac_key = Some(server_hmac_key);
            e.encrypt_key = Some(client_key);
            e.send_hmac_key = Some(client_hmac_key);
            e.pin_code = Some(to_four_digit_string(&auth_string));
            e.encryption_done = true;
        });

        Ok(())
    }

    async fn send_ukey2_alert(&mut self, atype: AlertType) -> Result<(), anyhow::Error> {
        let alert = Ukey2Alert {
            r#type: Some(atype.into()),
            error_message: None,
        };

        // An ALERT message carrying the alert; upstream put the alert type in
        // the message type field, which no peer understands.
        let data = Ukey2Message {
            message_type: Some(ukey2_message::Type::Alert.into()),
            message_data: Some(alert.encode_to_vec()),
        };

        self.send_frame(data.encode_to_vec()).await
    }

    async fn send_encrypted_frame(
        &mut self,
        frame: &sharing_nearby::Frame,
    ) -> Result<(), anyhow::Error> {
        let frame_data = frame.encode_to_vec();
        let body_size = frame_data.len();

        let payload_header = PayloadHeader {
            id: Some(rand::rng().random_range(i64::MIN..i64::MAX)),
            r#type: Some(payload_header::PayloadType::Bytes.into()),
            total_size: Some(body_size as i64),
            is_sensitive: Some(false),
            ..Default::default()
        };

        let transfer = PayloadTransferFrame {
            packet_type: Some(PacketType::Data.into()),
            payload_chunk: Some(PayloadChunk {
                offset: Some(0),
                flags: Some(0),
                body: Some(frame_data),
                index: Some(0),
                ..Default::default()
            }),
            payload_header: Some(payload_header.clone()),
            ..Default::default()
        };

        let wrapper = location_nearby_connections::OfflineFrame {
            version: Some(location_nearby_connections::offline_frame::Version::V1.into()),
            v1: Some(location_nearby_connections::V1Frame {
                r#type: Some(
                    location_nearby_connections::v1_frame::FrameType::PayloadTransfer.into(),
                ),
                payload_transfer: Some(transfer),
                ..Default::default()
            }),
        };

        // Encrypt and send offline
        self.encrypt_and_send(&wrapper).await?;

        // Send lastChunk
        let transfer = PayloadTransferFrame {
            packet_type: Some(PacketType::Data.into()),
            payload_chunk: Some(PayloadChunk {
                offset: Some(body_size as i64),
                flags: Some(1), // lastChunk
                body: Some(vec![]),
                index: Some(1),
                ..Default::default()
            }),
            payload_header: Some(payload_header),
            ..Default::default()
        };

        let wrapper = location_nearby_connections::OfflineFrame {
            version: Some(location_nearby_connections::offline_frame::Version::V1.into()),
            v1: Some(location_nearby_connections::V1Frame {
                r#type: Some(
                    location_nearby_connections::v1_frame::FrameType::PayloadTransfer.into(),
                ),
                payload_transfer: Some(transfer),
                ..Default::default()
            }),
        };

        // Encrypt and send offline
        self.encrypt_and_send(&wrapper).await?;

        Ok(())
    }

    async fn encrypt_and_send(&mut self, frame: &OfflineFrame) -> Result<(), anyhow::Error> {
        let d2d_msg = DeviceToDeviceMessage {
            sequence_number: Some(self.get_server_seq_inc()?),
            message: Some(frame.encode_to_vec()),
        };

        let key = self
            .state
            .encrypt_key
            .as_ref()
            .ok_or_else(|| anyhow!("no keys yet"))?;
        let msg_data = d2d_msg.encode_to_vec();
        let iv = gen_random(16);

        let encrypted = aes_cbc_encrypt(key, &iv, &msg_data)?;

        let hb = HeaderAndBody {
            body: encrypted,
            header: Header {
                encryption_scheme: EncScheme::Aes256Cbc.into(),
                signature_scheme: SigScheme::HmacSha256.into(),
                iv: Some(iv),
                public_metadata: Some(
                    GcmMetadata {
                        r#type: Type::DeviceToDeviceMessage.into(),
                        version: Some(1),
                    }
                    .encode_to_vec(),
                ),
                ..Default::default()
            },
        };

        let hmac_key = self
            .state
            .send_hmac_key
            .as_ref()
            .ok_or_else(|| anyhow!("no keys yet"))?;
        let mut hmac = HmacSha256::new_from_slice(hmac_key)?;
        hmac.update(&hb.encode_to_vec());
        let result = hmac.finalize();

        let smsg = SecureMessage {
            header_and_body: hb.encode_to_vec(),
            signature: result.into_bytes().to_vec(),
        };

        self.send_frame(smsg.encode_to_vec()).await?;

        Ok(())
    }

    async fn send_keepalive(&mut self, ack: bool) -> Result<(), anyhow::Error> {
        let ack_frame = location_nearby_connections::OfflineFrame {
            version: Some(location_nearby_connections::offline_frame::Version::V1.into()),
            v1: Some(location_nearby_connections::V1Frame {
                r#type: Some(location_nearby_connections::v1_frame::FrameType::KeepAlive.into()),
                keep_alive: Some(KeepAliveFrame { ack: Some(ack) }),
                ..Default::default()
            }),
        };

        if self.state.encryption_done {
            self.encrypt_and_send(&ack_frame).await
        } else {
            self.send_frame(ack_frame.encode_to_vec()).await
        }
    }

    async fn send_frame(&mut self, data: Vec<u8>) -> Result<(), anyhow::Error> {
        write_frame(&mut self.socket, &data).await
    }

    // Sequence numbers are i32 and only count up; one that would wrap ends
    // the connection rather than overflowing (a panic with overflow checks,
    // a repeated number without).
    fn get_server_seq_inc(&mut self) -> Result<i32, anyhow::Error> {
        self.state.server_seq = self
            .state
            .server_seq
            .checked_add(1)
            .ok_or_else(|| anyhow!("sequence number exhausted"))?;
        Ok(self.state.server_seq)
    }

    fn get_client_seq_inc(&mut self) -> Result<i32, anyhow::Error> {
        self.state.client_seq = self
            .state
            .client_seq
            .checked_add(1)
            .ok_or_else(|| anyhow!("sequence number exhausted"))?;
        Ok(self.state.client_seq)
    }

    fn update_state<F>(&mut self, f: F)
    where
        F: FnOnce(&mut InnerState),
    {
        f(&mut self.state);
    }
}
