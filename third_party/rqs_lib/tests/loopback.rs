//! A sender and a receiver from this crate, talking over an in-memory pipe:
//! the whole handshake, consent, and the payload, driven the way an
//! embedding application drives them.

use rqs_lib::hdl::TextPayloadType;
use rqs_lib::hdl::info::{OutgoingFile, OutgoingText};
use rqs_lib::{InboundEvent, InboundRequest, OutboundEvent, OutboundPayload, OutboundRequest};

async fn receive(
    socket: tokio::io::DuplexStream,
    accept: bool,
) -> (Vec<(String, Vec<u8>)>, Option<String>, Option<String>) {
    let mut ir = InboundRequest::new(socket);
    let intro = loop {
        match ir.next_event().await.unwrap() {
            Some(InboundEvent::Introduction(i)) => break i,
            None => {}
            Some(other) => panic!("unexpected {other:?}"),
        }
    };
    let pin = ir.pin_code().map(str::to_owned);
    if !accept {
        ir.reject_transfer(None).await.unwrap();
        return (Vec::new(), None, pin);
    }
    ir.accept_transfer().await.unwrap();
    let mut files: Vec<(String, Vec<u8>)> = intro
        .files
        .iter()
        .map(|f| (f.name.clone(), Vec::new()))
        .collect();
    let mut text = None;
    while !ir.is_finished() {
        match ir.next_event().await.unwrap() {
            Some(InboundEvent::FileChunk(c)) => {
                let i = intro
                    .files
                    .iter()
                    .position(|f| f.payload_id == c.payload_id)
                    .unwrap();
                assert_eq!(c.offset as usize, files[i].1.len());
                files[i].1.extend_from_slice(&c.body);
            }
            Some(InboundEvent::Text { text: t, .. }) => text = Some(t),
            None => {}
            Some(other) => panic!("unexpected {other:?}"),
        }
    }
    ir.disconnection().await.unwrap();
    (files, text, pin)
}

async fn send(
    socket: tokio::io::DuplexStream,
    payload: OutboundPayload,
    contents: Vec<Vec<u8>>,
) -> (bool, Option<String>) {
    let mut or = OutboundRequest::new(*b"ABCD", socket, "Sender".into(), payload.clone());
    or.send_connection_request().await.unwrap();
    or.send_ukey2_client_init().await.unwrap();
    let accepted = loop {
        match or.next_event().await.unwrap() {
            Some(OutboundEvent::IntroductionSent) | None => {}
            Some(OutboundEvent::Accepted) => break true,
            Some(OutboundEvent::Rejected(_)) => break false,
            Some(other) => panic!("unexpected {other:?}"),
        }
    };
    let pin = or.pin_code().map(str::to_owned);
    if !accepted {
        return (false, pin);
    }
    match payload {
        OutboundPayload::Files(_) => {
            let ids = or.file_ids().to_vec();
            for (id, content) in ids.into_iter().zip(contents) {
                for chunk in content.chunks(1000) {
                    or.send_file_chunk(id, chunk).await.unwrap();
                }
                or.finish_file(id).await.unwrap();
            }
        }
        OutboundPayload::Text(_) => or.send_text().await.unwrap(),
    }
    or.finish().await.unwrap();
    (true, pin)
}

fn file(name: &str, content: &[u8]) -> OutgoingFile {
    OutgoingFile {
        name: name.into(),
        size: content.len() as i64,
        mime_type: "application/octet-stream".into(),
    }
}

#[tokio::test]
async fn files_go_through() {
    let (a, b) = tokio::io::duplex(8192);
    let contents = vec![vec![1u8; 2500], Vec::new(), b"hello".to_vec()];
    let payload = OutboundPayload::Files(vec![
        file("a.bin", &contents[0]),
        file("empty", &contents[1]),
        file("c.txt", &contents[2]),
    ]);
    let (sent, received) = tokio::join!(send(a, payload, contents.clone()), receive(b, true));
    assert!(sent.0);
    let (files, text, pin) = received;
    assert_eq!(text, None);
    assert_eq!(files.len(), 3);
    assert_eq!(files[0], ("a.bin".to_string(), contents[0].clone()));
    assert_eq!(files[1], ("empty".to_string(), Vec::new()));
    assert_eq!(files[2], ("c.txt".to_string(), contents[2].clone()));
    assert_eq!(pin.as_deref().map(str::len), Some(4));
    assert_eq!(pin, sent.1, "both sides show the same PIN");
}

#[tokio::test]
async fn text_goes_through() {
    let (a, b) = tokio::io::duplex(8192);
    let payload = OutboundPayload::Text(OutgoingText {
        kind: TextPayloadType::Text,
        title: "hi".into(),
        text: "hi there, äöü".into(),
    });
    let (sent, received) = tokio::join!(send(a, payload, Vec::new()), receive(b, true));
    assert!(sent.0);
    assert_eq!(received.1.as_deref(), Some("hi there, äöü"));
}

#[tokio::test]
async fn a_rejection_reaches_the_sender() {
    let (a, b) = tokio::io::duplex(8192);
    let payload = OutboundPayload::Files(vec![file("a", b"x")]);
    let (sent, _) = tokio::join!(send(a, payload, vec![b"x".to_vec()]), receive(b, false));
    assert!(!sent.0);
}

#[tokio::test]
async fn file_bytes_before_consent_are_refused() {
    let (a, b) = tokio::io::duplex(8192);
    let sender = async move {
        let payload = OutboundPayload::Files(vec![file("a", b"xyz")]);
        let mut or = OutboundRequest::new(*b"ABCD", a, "Sender".into(), payload);
        or.send_connection_request().await.unwrap();
        or.send_ukey2_client_init().await.unwrap();
        while or.next_event().await.unwrap() != Some(OutboundEvent::IntroductionSent) {}
        // Straight on, without waiting for the answer.
        let id = or.file_ids()[0];
        let _ = or.send_file_chunk(id, b"xyz").await;
        or
    };
    let receiver = async move {
        let mut ir = InboundRequest::new(b);
        loop {
            match ir.next_event().await {
                Ok(Some(InboundEvent::Introduction(_))) => break,
                Ok(_) => {}
                Err(e) => panic!("{e}"),
            }
        }
        // The user has not answered; the early chunk ends the connection.
        loop {
            match ir.next_event().await {
                Ok(None) => {}
                other => break other,
            }
        }
    };
    let (_or, early) = tokio::join!(sender, receiver);
    assert!(
        early
            .unwrap_err()
            .to_string()
            .contains("before the transfer was accepted")
    );
}
