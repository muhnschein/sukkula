//! The receiver's own checks, driven from inside: a request put in the
//! state it has once the handshake is done, fed the frames a sender could
//! send next.

use tokio::io::DuplexStream;

use super::*;
use crate::sharing_nearby::{
    FileMetadata, IntroductionFrame, TextMetadata, WifiCredentialsMetadata,
};

/// A receiver past the handshake, and the other end of its socket (kept
/// open, with room for everything it writes).
fn ready(state: TransferState) -> (InboundRequest<DuplexStream>, DuplexStream) {
    let (ours, theirs) = tokio::io::duplex(1 << 20);
    let mut ir = InboundRequest::new(ours);
    ir.state.state = state;
    ir.state.encryption_done = true;
    ir.state.encrypt_key = Some(vec![1; 32]);
    ir.state.send_hmac_key = Some(vec![2; 32]);
    ir.state.decrypt_key = Some(vec![3; 32]);
    ir.state.recv_hmac_key = Some(vec![4; 32]);
    (ir, theirs)
}

fn introduction(intro: IntroductionFrame) -> sharing_nearby::Frame {
    sharing_nearby::Frame {
        version: Some(sharing_nearby::frame::Version::V1.into()),
        v1: Some(sharing_nearby::V1Frame {
            r#type: Some(sharing_nearby::v1_frame::FrameType::Introduction.into()),
            introduction: Some(intro),
            ..Default::default()
        }),
    }
}

fn file(id: i64, name: &str, size: i64) -> FileMetadata {
    FileMetadata {
        payload_id: Some(id),
        name: Some(name.into()),
        size: Some(size),
        ..Default::default()
    }
}

fn chunk(
    kind: payload_header::PayloadType,
    id: i64,
    total: i64,
    offset: i64,
    body: &[u8],
    last: bool,
) -> OfflineFrame {
    OfflineFrame {
        version: Some(location_nearby_connections::offline_frame::Version::V1.into()),
        v1: Some(location_nearby_connections::V1Frame {
            r#type: Some(location_nearby_connections::v1_frame::FrameType::PayloadTransfer.into()),
            payload_transfer: Some(PayloadTransferFrame {
                packet_type: Some(PacketType::Data.into()),
                payload_header: Some(PayloadHeader {
                    id: Some(id),
                    r#type: Some(kind.into()),
                    total_size: Some(total),
                    ..Default::default()
                }),
                payload_chunk: Some(PayloadChunk {
                    offset: Some(offset),
                    flags: Some(i32::from(last)),
                    body: Some(body.to_vec()),
                    ..Default::default()
                }),
                ..Default::default()
            }),
            ..Default::default()
        }),
    }
}

async fn introduced(files: Vec<FileMetadata>) -> (InboundRequest<DuplexStream>, DuplexStream) {
    let (mut ir, peer) = ready(TransferState::ReceivedPairedKeyResult);
    ir.process_transfer_setup(&introduction(IntroductionFrame {
        file_metadata: files,
        ..Default::default()
    }))
    .await
    .unwrap();
    (ir, peer)
}

#[tokio::test]
async fn names_reach_the_application_as_sent_and_nothing_else() {
    let (mut ir, _peer) = ready(TransferState::ReceivedPairedKeyResult);
    let names = [
        "../../.bashrc",
        "/etc/passwd",
        "..",
        "a\\b/c",
        "\u{202e}gpj.exe",
    ];
    let files = (1i64..).zip(names).map(|(id, n)| file(id, n, 3)).collect();
    let ev = ir
        .process_transfer_setup(&introduction(IntroductionFrame {
            file_metadata: files,
            ..Default::default()
        }))
        .await
        .unwrap();
    let Some(InboundEvent::Introduction(intro)) = ev else {
        panic!("{ev:?}")
    };
    // Untouched: choosing a file name is the application's job, and the
    // library has no directory to join them onto.
    let got: Vec<&str> = intro.files.iter().map(|f| f.name.as_str()).collect();
    assert_eq!(got, names);
    assert_eq!(ir.state.state, TransferState::WaitingForUserConsent);
}

#[tokio::test]
async fn wifi_credentials_are_refused_before_consent() {
    for with_files in [false, true] {
        let (mut ir, _peer) = ready(TransferState::ReceivedPairedKeyResult);
        let intro = IntroductionFrame {
            file_metadata: if with_files {
                vec![file(1, "a", 1)]
            } else {
                vec![]
            },
            wifi_credentials_metadata: vec![WifiCredentialsMetadata {
                ssid: Some("net".into()),
                ..Default::default()
            }],
            ..Default::default()
        };
        let err = ir
            .process_transfer_setup(&introduction(intro))
            .await
            .unwrap_err();
        assert!(err.to_string().contains("Wi-Fi"), "{err}");
    }
}

#[tokio::test]
async fn bad_file_lists_are_refused_before_consent() {
    let too_many = (0..=MAX_INTRODUCTION_FILES as i64)
        .map(|i| file(i, "f", 1))
        .collect();
    for files in [
        vec![file(1, "a", -1)],
        vec![file(1, "a", i64::MIN)],
        vec![file(1, "a", 1), file(1, "b", 1)],
        too_many,
    ] {
        let (mut ir, _peer) = ready(TransferState::ReceivedPairedKeyResult);
        let intro = IntroductionFrame {
            file_metadata: files,
            ..Default::default()
        };
        assert!(
            ir.process_transfer_setup(&introduction(intro))
                .await
                .is_err()
        );
    }
    for size in [-1, MAX_TEXT_PAYLOAD_LENGTH + 1] {
        let (mut ir, _peer) = ready(TransferState::ReceivedPairedKeyResult);
        let intro = IntroductionFrame {
            text_metadata: vec![TextMetadata {
                payload_id: Some(9),
                size: Some(size),
                r#type: Some(text_metadata::Type::Text.into()),
                ..Default::default()
            }],
            ..Default::default()
        };
        assert!(
            ir.process_transfer_setup(&introduction(intro))
                .await
                .is_err()
        );
    }
}

#[tokio::test]
async fn no_payload_byte_before_acceptance() {
    let (mut ir, _peer) = introduced(vec![file(7, "a", 3)]).await;
    let early = chunk(payload_header::PayloadType::File, 7, 3, 0, b"abc", true);
    let err = ir.process_offline_frame(early).await.unwrap_err();
    assert!(err.to_string().contains("before the transfer was accepted"));
}

#[tokio::test]
async fn file_chunks_stop_at_the_declared_size() {
    let file_kind = payload_header::PayloadType::File;
    let (mut ir, _peer) = introduced(vec![file(7, "a", 3)]).await;
    ir.accept_transfer().await.unwrap();
    let ok = ir
        .process_offline_frame(chunk(file_kind, 7, 3, 0, b"ab", false))
        .await
        .unwrap();
    assert!(matches!(ok, Some(InboundEvent::FileChunk(ref c)) if c.body == b"ab"));
    // Past the end, a wrong offset, an unknown id.
    for bad in [
        chunk(file_kind, 7, 3, 2, b"cd", false),
        chunk(file_kind, 7, 3, 0, b"c", false),
        chunk(file_kind, 8, 3, 0, b"c", false),
    ] {
        assert!(ir.process_offline_frame(bad).await.is_err());
    }
    // A last chunk short of the declared size.
    let (mut ir, _peer) = introduced(vec![file(7, "a", 3)]).await;
    ir.accept_transfer().await.unwrap();
    let short = chunk(file_kind, 7, 3, 0, b"a", true);
    assert!(ir.process_offline_frame(short).await.is_err());
}

#[tokio::test]
async fn byte_payloads_are_bounded() {
    let bytes = payload_header::PayloadType::Bytes;
    // Q3: a negative size; Q4: more than declared, or over the cap.
    for bad in [
        chunk(bytes, 1, -1, 0, b"", true),
        chunk(bytes, 2, 2, 0, b"abc", false),
        chunk(bytes, 3, MAX_CONTROL_PAYLOAD_LENGTH + 1, 0, b"a", false),
    ] {
        let (mut ir, _peer) = ready(TransferState::SentConnectionResponse);
        assert!(ir.process_offline_frame(bad).await.is_err());
        assert!(ir.state.payload_buffers.is_empty());
    }
}
