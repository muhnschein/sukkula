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
use super::info::{FileChunk, IncomingFile, IncomingText, InternalFileInfo, Introduction};
use super::payload::{
    MAX_CONTROL_PAYLOAD_LENGTH, MAX_INTRODUCTION_FILES, MAX_TEXT_PAYLOAD_LENGTH, assemble,
};
use super::{InnerState, TextPayloadInfo, TextPayloadType, TransferState};
use crate::location_nearby_connections::payload_transfer_frame::{
    PacketType, PayloadChunk, PayloadHeader, payload_header,
};
use crate::location_nearby_connections::{KeepAliveFrame, OfflineFrame, PayloadTransferFrame};
use crate::securegcm::ukey2_alert::AlertType;
use crate::securegcm::{
    DeviceToDeviceMessage, GcmMetadata, Type, Ukey2Alert, Ukey2ClientFinished, Ukey2ClientInit,
    Ukey2HandshakeCipher, Ukey2Message, Ukey2ServerInit, ukey2_message,
};
use crate::securemessage::{
    EcP256PublicKey, EncScheme, GenericPublicKey, Header, HeaderAndBody, PublicKeyType,
    SecureMessage, SigScheme,
};
use crate::sharing_nearby::{paired_key_result_frame, text_metadata};
use crate::utils::{
    DeviceType, RemoteDeviceInfo, aes_cbc_decrypt, aes_cbc_encrypt, decode_p256_point,
    encode_point, gen_ecdsa_keypair, gen_random, hkdf_extract_expand, to_four_digit_string,
};
use crate::{location_nearby_connections, sharing_nearby};

type HmacSha256 = Hmac<Sha256>;

/// What a receiving connection has for the embedding application.
///
/// rqs_lib does not touch the file system: the application decides whether
/// and where anything is written, and gets the bytes of each accepted file
/// in order, to write through its own storage code.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InboundEvent {
    /// The handshake is done and the sender says what it offers. Answer
    /// with [`InboundRequest::accept_transfer`] or
    /// [`InboundRequest::reject_transfer`].
    Introduction(Introduction),
    /// Bytes of an accepted file.
    FileChunk(FileChunk),
    /// The complete content of an accepted text.
    Text { payload_id: i64, text: String },
    /// The sender cancelled.
    Cancelled,
    /// The sender hung up.
    Disconnected,
}

/// The receiving side of one connection. The embedding application owns
/// the socket and the task, and drives the handshake one frame at a time:
/// [`read_frame`](Self::read_frame) (cancel-safe) and then
/// [`process_frame`](Self::process_frame), or both at once with
/// [`next_event`](Self::next_event).
#[derive(Debug)]
pub struct InboundRequest<S = TcpStream> {
    socket: S,
    reader: FrameReader,
    pub state: InnerState,
}

impl<S: AsyncRead + AsyncWrite + Unpin> InboundRequest<S> {
    pub fn new(socket: S) -> Self {
        Self {
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
        }
    }

    /// The sender, as its connection request described it (untrusted).
    pub fn remote_device_info(&self) -> Option<&RemoteDeviceInfo> {
        self.state.remote_device_info.as_ref()
    }

    /// The PIN both screens show, once the key exchange is done.
    pub fn pin_code(&self) -> Option<&str> {
        self.state.pin_code.as_deref()
    }

    /// Whether every accepted file (or the text) has arrived.
    pub fn is_finished(&self) -> bool {
        self.state.state == TransferState::Finished
    }

    /// Whether the next frame from the sender is still a plaintext
    /// handshake frame.
    fn is_handshaking(&self) -> bool {
        matches!(
            self.state.state,
            TransferState::Initial
                | TransferState::ReceivedConnectionRequest
                | TransferState::SentUkeyServerInit
                | TransferState::ReceivedUkeyClientFinish
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

    /// Reads and processes the next frame. Not cancel-safe: it may be
    /// halfway through writing a reply.
    pub async fn next_event(&mut self) -> Result<Option<InboundEvent>, anyhow::Error> {
        let frame = self.read_frame().await?;
        self.process_frame(frame).await
    }

    /// Processes one frame from [`read_frame`](Self::read_frame), answering
    /// it as the protocol requires. Not cancel-safe.
    pub async fn process_frame(
        &mut self,
        frame_data: Vec<u8>,
    ) -> Result<Option<InboundEvent>, anyhow::Error> {
        if frame_data.len() > SANE_FRAME_LENGTH {
            return Err(anyhow!("Message length too big"));
        }

        let current_state = &self.state;
        // Now determine what will be the request type based on current state
        match current_state.state {
            TransferState::Initial => {
                debug!("Handling State::Initial frame");
                let frame = location_nearby_connections::OfflineFrame::decode(&*frame_data)?;
                let rdi = self.process_connection_request(&frame)?;
                debug!("connection request from a {:?}", rdi.device_type);

                // Advance current state
                self.update_state(|e: &mut InnerState| {
                    e.state = TransferState::ReceivedConnectionRequest;
                    e.remote_device_info = Some(rdi);
                });
            }
            TransferState::ReceivedConnectionRequest => {
                debug!("Handling State::ReceivedConnectionRequest frame");
                let msg = Ukey2Message::decode(&*frame_data)?;
                self.process_ukey2_client_init(&msg).await?;

                self.update_state(|e: &mut InnerState| {
                    e.state = TransferState::SentUkeyServerInit;
                    e.client_init_msg_data = Some(frame_data);
                });
            }
            TransferState::SentUkeyServerInit => {
                debug!("Handling State::SentUkeyServerInit frame");
                let msg = Ukey2Message::decode(&*frame_data)?;
                self.process_ukey2_client_finish(&msg, &frame_data).await?;

                self.update_state(|e: &mut InnerState| {
                    e.state = TransferState::ReceivedUkeyClientFinish;
                });
            }
            TransferState::ReceivedUkeyClientFinish => {
                debug!("Handling State::ReceivedUkeyClientFinish frame");
                let frame = location_nearby_connections::OfflineFrame::decode(&*frame_data)?;
                self.process_connection_response(&frame).await?;

                self.update_state(|e: &mut InnerState| {
                    e.state = TransferState::SentConnectionResponse;
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

    fn process_connection_request(
        &mut self,
        frame: &location_nearby_connections::OfflineFrame,
    ) -> Result<RemoteDeviceInfo, anyhow::Error> {
        let v1_frame = frame
            .v1
            .as_ref()
            .ok_or_else(|| anyhow!("Missing required fields"))?;

        if v1_frame.r#type() != location_nearby_connections::v1_frame::FrameType::ConnectionRequest
        {
            return Err(anyhow!(format!(
                "Unexpected frame type: {:?}",
                v1_frame.r#type()
            )));
        }

        let connection_request = v1_frame
            .connection_request
            .as_ref()
            .ok_or_else(|| anyhow!("Missing required fields"))?;

        let endpoint_info = connection_request
            .endpoint_info
            .as_ref()
            .ok_or_else(|| anyhow!("Missing endpoint info"))?;

        // 1 byte of flags, 16 of identity, 1 of name length, then the name.
        let (Some(&flags), Some(&device_name_length)) =
            (endpoint_info.first(), endpoint_info.get(17))
        else {
            return Err(anyhow!("Endpoint info too short"));
        };
        let name_bytes = endpoint_info
            .get(18..18 + usize::from(device_name_length))
            .ok_or_else(|| anyhow!("Endpoint info too short to contain the device name"))?;

        // Extract and validate device name based on length
        let device_name = std::str::from_utf8(name_bytes)
            .map_err(|_| anyhow!("Device name is not valid UTF-8"))?;

        // Parsing the device type: bits 3..1 of the flags byte.
        let raw_device_type = (flags >> 1) & 7;

        Ok(RemoteDeviceInfo {
            name: device_name.to_string(),
            device_type: DeviceType::from_raw_value(raw_device_type),
        })
    }

    async fn process_ukey2_client_init(&mut self, msg: &Ukey2Message) -> Result<(), anyhow::Error> {
        if msg.message_type() != ukey2_message::Type::ClientInit {
            self.send_ukey2_alert(AlertType::BadMessageType).await?;
            return Err(anyhow!(
                "UKey2: message_type({:?}) != ClientInit",
                msg.message_type
            ));
        }

        let client_init = match Ukey2ClientInit::decode(msg.message_data()) {
            Ok(uk2ci) => uk2ci,
            Err(e) => {
                self.send_ukey2_alert(AlertType::BadMessageData).await?;
                return Err(anyhow!("UKey2: Ukey2ClientInit::decode: {}", e));
            }
        };

        if client_init.version() != 1 {
            self.send_ukey2_alert(AlertType::BadVersion).await?;
            return Err(anyhow!("UKey2: client_init.version != 1"));
        }

        if client_init.random().len() != 32 {
            self.send_ukey2_alert(AlertType::BadRandom).await?;
            return Err(anyhow!("UKey2: client_init.random.len != 32"));
        }

        // Searching for preferred cipher commitment
        let mut found = false;
        for commitment in &client_init.cipher_commitments {
            trace!("CipherCommitment: {:?}", commitment.handshake_cipher());
            if Ukey2HandshakeCipher::P256Sha512 == commitment.handshake_cipher() {
                found = true;
                self.update_state(|e| {
                    e.cipher_commitment = Some(commitment.clone());
                });
                break;
            }
        }

        if !found {
            self.send_ukey2_alert(AlertType::BadHandshakeCipher).await?;
            return Err(anyhow!("UKey2: badHandshakeCipher"));
        }

        if client_init.next_protocol() != "AES_256_CBC-HMAC_SHA256" {
            self.send_ukey2_alert(AlertType::BadNextProtocol).await?;
            return Err(anyhow!("UKey2: badNextProtocol"));
        }

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

        let server_init = Ukey2ServerInit {
            version: Some(1),
            random: Some(rand::rng().random::<[u8; 32]>().to_vec()),
            handshake_cipher: Some(Ukey2HandshakeCipher::P256Sha512.into()),
            public_key: Some(pkey.encode_to_vec()),
        };

        let server_init_msg = Ukey2Message {
            message_type: Some(ukey2_message::Type::ServerInit.into()),
            message_data: Some(server_init.encode_to_vec()),
        };

        let server_init_data = server_init_msg.encode_to_vec();
        self.update_state(|e| {
            e.private_key = Some(secret_key);
            e.public_key = Some(public_key);
            e.server_init_data = Some(server_init_data.clone());
        });

        self.send_frame(server_init_data).await?;

        Ok(())
    }

    async fn process_ukey2_client_finish(
        &mut self,
        msg: &Ukey2Message,
        frame_data: &Vec<u8>,
    ) -> Result<(), anyhow::Error> {
        if msg.message_type() != ukey2_message::Type::ClientFinish {
            self.send_ukey2_alert(AlertType::BadMessageType).await?;
            return Err(anyhow!(
                "UKey2: message_type({:?}) != ClientFinish",
                msg.message_type
            ));
        }

        let sha512 = Sha512::digest(frame_data);
        let commitment = self
            .state
            .cipher_commitment
            .as_ref()
            .ok_or_else(|| anyhow!("UKey2: no commitment"))?;
        if commitment.commitment() != &sha512[..] {
            error!("cipher_commitment isn't equals to sha512(frame_data)");
            return Err(anyhow!("UKey2: cipher_commitment != sha512"));
        }

        let client_finish = match Ukey2ClientFinished::decode(msg.message_data()) {
            Ok(uk2cf) => uk2cf,
            Err(e) => {
                return Err(anyhow!("UKey2: Ukey2ClientFinished::decode: {}", e));
            }
        };

        if client_finish.public_key.is_none() {
            return Err(anyhow!("UKey2: client_finish.public_key None"));
        }

        let client_public_key = match GenericPublicKey::decode(client_finish.public_key()) {
            Ok(cpk) => cpk,
            Err(e) => {
                return Err(anyhow!("UKey2: GenericPublicKey::decode: {}", e));
            }
        };

        self.finalize_key_exchange(client_public_key).await?;

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

        let response = location_nearby_connections::OfflineFrame {
            version: Some(location_nearby_connections::offline_frame::Version::V1.into()),
            v1: Some(location_nearby_connections::V1Frame {
                r#type: Some(
                    location_nearby_connections::v1_frame::FrameType::ConnectionResponse.into(),
                ),
                connection_response: Some(location_nearby_connections::ConnectionResponseFrame {
                    response: Some(
                        location_nearby_connections::connection_response_frame::ResponseStatus::Accept
                            .into(),
                    ),
                    os_info: Some(location_nearby_connections::OsInfo {
                        r#type: Some(location_nearby_connections::os_info::OsType::Linux.into()),
                    }),
                    ..Default::default()
                }),
                ..Default::default()
            }),
        };

        self.send_frame(response.encode_to_vec()).await?;

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
    ) -> Result<Option<InboundEvent>, anyhow::Error> {
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

        let offline = location_nearby_connections::OfflineFrame::decode(d2d_msg.message())?;
        self.process_offline_frame(offline).await
    }

    /// Dispatch a decrypted OfflineFrame (payload transfers, keep-alives, …).
    async fn process_offline_frame(
        &mut self,
        offline: OfflineFrame,
    ) -> Result<Option<InboundEvent>, anyhow::Error> {
        let v1_frame = offline
            .v1
            .as_ref()
            .ok_or_else(|| anyhow!("Missing required fields"))?;
        debug!(
            "inbound frame: type={:?} (raw={:?})",
            v1_frame.r#type(),
            v1_frame.r#type
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

                // Interop forensics: a working peer's exact chunk shape is the
                // template for our own sender (Windows is stricter than
                // Android about these fields).
                // No file name or folder: names are the user's data.
                trace!(
                    "payload frame: id={} type={:?} total_size={:?} sensitive={:?} | chunk offset={:?} index={:?} flags={:?} body_len={}",
                    header.id(),
                    header.r#type(),
                    header.total_size,
                    header.is_sensitive,
                    chunk.offset,
                    chunk.index,
                    chunk.flags,
                    chunk.body.as_ref().map(|b| b.len()).unwrap_or(0)
                );

                match header.r#type() {
                    payload_header::PayloadType::Bytes => {
                        trace!("Processing PayloadType::Bytes");
                        let payload_id = header.id();

                        let is_text = self
                            .state
                            .text_payload
                            .as_ref()
                            .is_some_and(|t| t.get_i64_value() == payload_id);
                        // Consent first: the text's bytes are only taken once the
                        // user said yes. They used to be taken at any time, and
                        // a text that arrived before the answer was reported as
                        // a finished transfer.
                        if is_text && self.state.state != TransferState::ReceivingFiles {
                            return Err(anyhow!("text payload before the transfer was accepted"));
                        }
                        let max = if is_text {
                            MAX_TEXT_PAYLOAD_LENGTH
                        } else {
                            MAX_CONTROL_PAYLOAD_LENGTH
                        };
                        let assembled = assemble(
                            &mut self.state.payload_buffers,
                            payload_id,
                            header.total_size(),
                            max,
                            chunk,
                        )?;

                        if let Some(buffer) = assembled {
                            debug!("Chunk flags & 1 == 1 ?? End of data ??");

                            if is_text {
                                info!("Transfer finished");
                                let text = String::from_utf8(buffer)?;
                                self.update_state(|e| {
                                    e.state = TransferState::Finished;
                                });
                                return Ok(Some(InboundEvent::Text { payload_id, text }));
                            } else {
                                let inner_frame = sharing_nearby::Frame::decode(buffer.as_slice())?;
                                return self.process_transfer_setup(&inner_frame).await;
                            }
                        }
                    }
                    payload_header::PayloadType::File => {
                        trace!("Processing PayloadType::File");
                        let payload_id = header.id();

                        // Consent first: no file byte is taken before the user
                        // accepted. A chunk that came early used to reach
                        // `file.as_ref().unwrap()` on a file not yet created,
                        // and panic.
                        if self.state.state != TransferState::ReceivingFiles {
                            return Err(anyhow!("file payload before the transfer was accepted"));
                        }

                        let file_internal = self
                            .state
                            .transferred_files
                            .get_mut(&payload_id)
                            .ok_or_else(|| {
                                anyhow!("File payload ID ({}) is not known", payload_id)
                            })?;

                        let current_offset = file_internal.bytes_transferred;
                        if chunk.offset() != current_offset {
                            return Err(anyhow!(
                                "Invalid offset into file {}, expected {}",
                                chunk.offset(),
                                current_offset
                            ));
                        }

                        let chunk_size = i64::try_from(chunk.body().len())?;
                        let after = current_offset
                            .checked_add(chunk_size)
                            .ok_or_else(|| anyhow!("file offset overflow"))?;
                        if after > file_internal.total_size {
                            return Err(anyhow!(
                                "Transferred file size exceeds previously specified value: {} vs {}",
                                after,
                                file_internal.total_size
                            ));
                        }
                        let last = (chunk.flags() & 1) == 1;
                        if last && after != file_internal.total_size {
                            return Err(anyhow!(
                                "file ended after {} of {} bytes",
                                after,
                                file_internal.total_size
                            ));
                        }

                        // The bytes go to the application; rqs_lib writes no
                        // file (see InboundEvent).
                        file_internal.bytes_transferred = after;
                        if last {
                            self.state.transferred_files.remove(&payload_id);
                            if self.state.transferred_files.is_empty() {
                                info!("Transfer finished");
                                self.update_state(|e| {
                                    e.state = TransferState::Finished;
                                });
                            }
                        }
                        return Ok(Some(InboundEvent::FileChunk(FileChunk {
                            payload_id,
                            offset: current_offset,
                            body: chunk.body().to_vec(),
                            last,
                        })));
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
                return Ok(Some(InboundEvent::Disconnected));
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
    ) -> Result<Option<InboundEvent>, anyhow::Error> {
        let v1_frame = frame
            .v1
            .as_ref()
            .ok_or_else(|| anyhow!("Missing required fields"))?;

        debug!(
            "process_transfer_setup: state={:?} inner frame type={:?}",
            self.state.state,
            v1_frame.r#type()
        );

        if v1_frame.r#type() == sharing_nearby::v1_frame::FrameType::Cancel {
            info!("Transfer canceled");
            self.update_state(|e| {
                e.state = TransferState::Cancelled;
            });
            self.disconnection().await?;
            return Ok(Some(InboundEvent::Cancelled));
        }

        match self.state.state {
            TransferState::SentConnectionResponse => {
                debug!("Processing State::SentConnectionResponse");
                self.process_paired_key_encryption_frame(v1_frame).await?;
                self.update_state(|e| {
                    e.state = TransferState::SentPairedKeyResult;
                });
            }
            TransferState::SentPairedKeyResult => {
                debug!("Processing State::SentPairedKeyResult");
                self.process_paired_key_result(v1_frame).await?;
                self.update_state(|e| {
                    e.state = TransferState::ReceivedPairedKeyResult;
                });
            }
            TransferState::ReceivedPairedKeyResult => {
                debug!("Processing State::ReceivedPairedKeyResult");
                // Newer senders (Pixel) may interleave extra frames before the
                // introduction; wait for the actual introduction instead of erroring.
                if v1_frame.introduction.is_some() {
                    return self.process_introduction(v1_frame).await.map(Some);
                } else {
                    debug!(
                        "Awaiting introduction; ignoring frame type={:?}",
                        v1_frame.r#type()
                    );
                }
            }
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

    async fn process_paired_key_result(
        &self,
        v1_frame: &sharing_nearby::V1Frame,
    ) -> Result<(), anyhow::Error> {
        if v1_frame.paired_key_result.is_none() {
            return Err(anyhow!("Missing required fields"));
        }

        Ok(())
    }

    async fn process_introduction(
        &mut self,
        v1_frame: &sharing_nearby::V1Frame,
    ) -> Result<InboundEvent, anyhow::Error> {
        let introduction = v1_frame
            .introduction
            .as_ref()
            .ok_or_else(|| anyhow!("Missing required fields"))?;

        self.update_state(|e| {
            e.state = TransferState::WaitingForUserConsent;
        });

        // Wi-Fi credentials are never taken (Sukkula F-QS5): joining a network
        // a peer chose is a network change driven by the peer, and the
        // password parser below it logged the password on any parse error.
        // An introduction that carries any is refused whole, before consent.
        if !introduction.wifi_credentials_metadata.is_empty() {
            self.reject_transfer(Some(
                sharing_nearby::connection_response_frame::Status::UnsupportedAttachmentType,
            ))
            .await?;
            return Err(anyhow!("Wi-Fi credentials are not accepted"));
        }

        if !introduction.file_metadata.is_empty() && introduction.text_metadata.is_empty() {
            trace!("process_introduction: handling file_metadata");
            // Sizes are the peer's claim: a negative one used to wrap the
            // total announced to the user (`as u64`), and a duplicate
            // payload id made two files share one book-keeping entry.
            let files_ok = introduction.file_metadata.len() <= MAX_INTRODUCTION_FILES
                && introduction.file_metadata.iter().all(|f| f.size() >= 0);
            let mut ids = std::collections::HashSet::new();
            let unique = introduction
                .file_metadata
                .iter()
                .all(|f| ids.insert(f.payload_id()));
            if !files_ok || !unique {
                self.reject_transfer(None).await?;
                return Err(anyhow!("malformed file list"));
            }
            let mut files = Vec::with_capacity(introduction.file_metadata.len());

            // Names, sizes and types go to the application exactly as sent;
            // choosing a file name and a place for it is the application's
            // business, and so is refusing an offer it does not like.
            for file in &introduction.file_metadata {
                let info = InternalFileInfo {
                    payload_id: file.payload_id(),
                    bytes_transferred: 0,
                    total_size: file.size(),
                    chunks: 0,
                };
                self.state.transferred_files.insert(file.payload_id(), info);
                files.push(IncomingFile {
                    payload_id: file.payload_id(),
                    name: file.name().to_owned(),
                    size: file.size(),
                    mime_type: file.mime_type().to_owned(),
                });
            }

            Ok(InboundEvent::Introduction(Introduction {
                files,
                text: None,
            }))
        } else if introduction.text_metadata.len() == 1 {
            trace!("process_introduction: handling text_metadata");
            let Some(meta) = introduction.text_metadata.first() else {
                return Err(anyhow!("Missing required fields"));
            };
            if !(0..=MAX_TEXT_PAYLOAD_LENGTH).contains(&meta.size()) {
                self.reject_transfer(None).await?;
                return Err(anyhow!("text size out of range"));
            }

            let (kind, info) = match meta.r#type() {
                text_metadata::Type::Url => (
                    TextPayloadType::Url,
                    TextPayloadInfo::Url(meta.payload_id()),
                ),
                text_metadata::Type::PhoneNumber
                | text_metadata::Type::Address
                | text_metadata::Type::Text => (
                    TextPayloadType::Text,
                    TextPayloadInfo::Text(meta.payload_id()),
                ),
                text_metadata::Type::Unknown => {
                    // Reject transfer
                    self.reject_transfer(Some(
                        sharing_nearby::connection_response_frame::Status::UnsupportedAttachmentType,
                    ))
                    .await?;
                    return Err(anyhow!("unsupported text type"));
                }
            };
            self.update_state(|e| {
                e.text_payload = Some(info);
            });

            Ok(InboundEvent::Introduction(Introduction {
                files: Vec::new(),
                text: Some(IncomingText {
                    payload_id: meta.payload_id(),
                    kind,
                    title: meta.text_title().to_owned(),
                    size: meta.size(),
                }),
            }))
        } else {
            // Reject transfer
            self.reject_transfer(Some(
                sharing_nearby::connection_response_frame::Status::UnsupportedAttachmentType,
            ))
            .await?;
            Err(anyhow!("unsupported attachment type"))
        }
    }

    /// Says goodbye to the sender.
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

    /// Tells the sender to go ahead. Nothing is created or written here: the
    /// payload arrives as [`InboundEvent::FileChunk`]s and
    /// [`InboundEvent::Text`].
    pub async fn accept_transfer(&mut self) -> Result<(), anyhow::Error> {
        if self.state.state != TransferState::WaitingForUserConsent {
            return Err(anyhow!("nothing to accept"));
        }
        let frame = sharing_nearby::Frame {
            version: Some(sharing_nearby::frame::Version::V1.into()),
            v1: Some(sharing_nearby::V1Frame {
                r#type: Some(sharing_nearby::v1_frame::FrameType::Response.into()),
                connection_response: Some(sharing_nearby::ConnectionResponseFrame {
                    status: Some(sharing_nearby::connection_response_frame::Status::Accept.into()),
                }),
                ..Default::default()
            }),
        };

        self.send_encrypted_frame(&frame).await?;

        self.update_state(|e| {
            e.state = TransferState::ReceivingFiles;
        });

        Ok(())
    }

    /// Declines the offer, with `reason` (default: REJECT).
    pub async fn reject_transfer(
        &mut self,
        reason: Option<sharing_nearby::connection_response_frame::Status>,
    ) -> Result<(), anyhow::Error> {
        let sreason = if let Some(r) = reason {
            r
        } else {
            sharing_nearby::connection_response_frame::Status::Reject
        };

        let frame = sharing_nearby::Frame {
            version: Some(sharing_nearby::frame::Version::V1.into()),
            v1: Some(sharing_nearby::V1Frame {
                r#type: Some(sharing_nearby::v1_frame::FrameType::Response.into()),
                connection_response: Some(sharing_nearby::ConnectionResponseFrame {
                    status: Some(sreason.into()),
                }),
                ..Default::default()
            }),
        };

        self.send_encrypted_frame(&frame).await?;
        self.update_state(|e| {
            e.state = TransferState::Rejected;
        });

        Ok(())
    }

    /// Cancels a transfer in progress: a CANCEL frame, then goodbye.
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
            e.decrypt_key = Some(client_key);
            e.recv_hmac_key = Some(client_hmac_key);
            e.encrypt_key = Some(server_key);
            e.send_hmac_key = Some(server_hmac_key);
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
