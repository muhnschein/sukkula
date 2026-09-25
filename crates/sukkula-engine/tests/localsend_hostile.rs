//! Hostile LocalSend peers, with hand-built TLS and HTTP (spec §7 "Hostile
//! input"; clove's `tests/evil_peer.rs` is the model).
//!
//! What every scenario holds to:
//!
//! 1. no panic, no hang, no unbounded allocation -- every wait here has a
//!    deadline far shorter than the test's;
//! 2. nothing outside the download directory, nothing before consent, no
//!    partial file left behind (S1, S3, S5);
//! 3. what reaches the UI is sanitised (S2);
//! 4. a hostile peer cannot deny service to an honest one: scenarios end
//!    with an honest transfer through the same receiver.
//!
//! Distinct loopback source addresses (127.0.0.x) stand in for distinct
//! LAN peers, so per-address limits can be told apart.

#![cfg(feature = "localsend")]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    clippy::cast_possible_truncation,
    clippy::as_conversions
)]

mod localsend_support;

use std::collections::HashMap;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use localsend_support::*;
use serde_json::json;
use sukkula_core::limits::{MAX_FILE_BYTES, MAX_FILES_PER_OFFER};
use sukkula_core::name::is_safe;
use sukkula_engine::adapter::Adapter;
use sukkula_engine::api::{ErrorCode, Event, Outcome, SendTarget};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio_rustls::client::TlsStream;

/// The raw attacker's identity in the pool.
const EVE: usize = 3;

/// A receiver with short timeouts, so stalls resolve in seconds -- but not
/// so short that a loaded test machine trips them where no stall is meant.
fn receiver() -> Node {
    let mut cfg = NodeConfig::new(1, "Bob");
    cfg.idle = Duration::from_secs(3);
    cfg.handshake = Duration::from_secs(2);
    Node::new(&cfg)
}

fn src(last: u8) -> Option<Ipv4Addr> {
    Some(Ipv4Addr::new(127, 0, 0, last))
}

async fn tls_from(port: u16, source: Option<Ipv4Addr>) -> TlsStream<TcpStream> {
    raw_tls(port, Some(EVE), source).await.unwrap()
}

async fn post_from(
    port: u16,
    source: Option<Ipv4Addr>,
    path: &str,
    body: &[u8],
) -> Option<RawResponse> {
    let mut s = tls_from(port, source).await;
    exchange(&mut s, &post(path, body.len()), body).await
}

/// Offers `files` as Eve from `source`, has Bob accept, and returns the
/// session id and the file tokens.
async fn accepted(
    b: &Node,
    port: u16,
    source: Option<Ipv4Addr>,
    files: &serde_json::Value,
) -> (String, HashMap<String, String>) {
    let body = offer_json(EVE, "Eve", files);
    let shown = b.offers_shown();
    let answer = async {
        let (id, _) = b.offer_after(shown).await;
        b.answer(id, true);
    };
    let (response, ()) = tokio::join!(post_from(port, source, PREPARE, &body), answer);
    let response = response.unwrap();
    assert_eq!(
        response.status,
        200,
        "{}",
        String::from_utf8_lossy(&response.body)
    );
    let v = response.json();
    let session = v["sessionId"].as_str().unwrap().to_owned();
    let tokens = v["files"]
        .as_object()
        .unwrap()
        .iter()
        .map(|(k, t)| (k.clone(), t.as_str().unwrap().to_owned()))
        .collect();
    (session, tokens)
}

/// An upload head with a `Content-Length`.
fn upload_head(session: &str, file: &str, token: &str, len: usize) -> String {
    format!(
        "POST {} HTTP/1.1\r\nHost: 127.0.0.1\r\nContent-Length: {len}\r\n\r\n",
        upload_path(session, file, token)
    )
}

/// An upload head with chunked encoding.
fn chunked_head(path: &str) -> String {
    format!("POST {path} HTTP/1.1\r\nHost: 127.0.0.1\r\nTransfer-Encoding: chunked\r\n\r\n")
}

fn chunk(data: &[u8]) -> Vec<u8> {
    let mut out = format!("{:x}\r\n", data.len()).into_bytes();
    out.extend_from_slice(data);
    out.extend_from_slice(b"\r\n");
    out
}

/// Whether the peer closes `stream` within `within`: a read that ends.
async fn closed_within<S: tokio::io::AsyncRead + Unpin>(stream: &mut S, within: Duration) -> bool {
    let mut buf = [0u8; 256];
    let read = async {
        loop {
            match stream.read(&mut buf).await {
                Ok(0) | Err(_) => return,
                Ok(_) => {}
            }
        }
    };
    tokio::time::timeout(within, read).await.is_ok()
}

/// The honest peer the scenarios end with: Alice sends Bob a file and it
/// arrives.
async fn an_honest_transfer_still_works(b: &Node) {
    let a = Node::new(&NodeConfig::new(0, "Alice"));
    let peer = a.find(b).await;
    let dir = tempfile::tempdir().unwrap();
    let f = make_file(dir.path(), "honest.txt", 1234);
    let shown = b.offers_shown();
    let id =
        a.ls.send(SendTarget::LocalSend { peer }, vec![outgoing(&f)])
            .await
            .unwrap();
    let (offer, o) = b.offer_after(shown).await;
    assert_eq!(o.sender, "Alice");
    b.answer(offer, true);
    assert_eq!(a.finished(id).await.0, Outcome::Done);
    assert!(b.saved().contains(&"honest.txt".to_owned()));
    assert_eq!(b.partials(), 0);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn plain_http_and_certificateless_clients_get_nothing() {
    let b = receiver();
    let port = b.receive().await;
    let body = offer_json(EVE, "Eve", &json!({"f": file_json("f", "a", json!(1))}));

    // F-LS2: plain HTTP to the HTTPS port fails the handshake.
    let mut plain = tcp(port, None).await;
    plain
        .write_all(post(PREPARE, body.len()).as_bytes())
        .await
        .unwrap();
    let _ = plain.write_all(&body).await;
    let mut got = Vec::new();
    let _ = tokio::time::timeout(Duration::from_secs(3), plain.read_to_end(&mut got)).await;
    assert!(!got.starts_with(b"HTTP/"), "plain HTTP got an HTTP answer");

    // A TLS client without a certificate is never served.
    if let Ok(mut s) = raw_tls(port, None, None).await {
        assert!(
            exchange(&mut s, &post(PREPARE, body.len()), &body)
                .await
                .is_none()
        );
    }
    assert_eq!(b.offers_shown(), 0);
    an_honest_transfer_still_works(&b).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn unpermitted_addresses_are_dropped_before_the_handshake() {
    // Loopback is not a permitted address unless the test allows it: this
    // receiver stands for one on a LAN that hears from a public address.
    let mut cfg = NodeConfig::new(1, "Bob");
    cfg.allow_loopback = false;
    let b = Node::new(&cfg);
    let port = b.receive().await;
    let start = Instant::now();
    assert!(raw_tls(port, Some(EVE), None).await.is_err());
    assert!(start.elapsed() < Duration::from_secs(2), "dropped at once");
    let mut s = tcp(port, None).await;
    assert!(closed_within(&mut s, Duration::from_secs(2)).await);
    assert_eq!(b.offers_shown(), 0);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn hostile_names_are_sanitised_and_stay_in_the_inbox() {
    let b = receiver();
    let port = b.receive().await;
    let names = [
        "../x",
        "../../../../etc/passwd",
        "/etc/passwd",
        "a\\b\\..\\c",
        "..",
        ".hidden",
        "evil\u{202E}gpj.exe",
        "zero\u{200B}width.txt",
        "nul\u{0}byte",
        "tab\tand\nnewline",
        "",
        "   ",
        "trailing. ",
    ];
    let long = format!("{}.jpg", "x".repeat(300));
    let mut files = serde_json::Map::new();
    for (i, n) in names.iter().copied().chain([long.as_str()]).enumerate() {
        files.insert(format!("f{i}"), file_json(&format!("f{i}"), n, json!(3)));
    }
    let files = serde_json::Value::Object(files);
    let body = offer_json(EVE, "Eve\u{202E}\u{200B}\u{7}", &files);
    let answer = async {
        let (id, offer) = b.offer().await;
        // S1/S2 before the user sees anything.
        assert_eq!(offer.sender, "Eve");
        assert_eq!(offer.model.as_deref(), Some("Evil"));
        for f in &offer.files {
            assert!(is_safe(f.name.as_str()), "{:?}", f.name);
            assert!(f.name.as_str().len() <= 200);
        }
        b.answer(id, true);
    };
    let (response, ()) = tokio::join!(post_from(port, None, PREPARE, &body), answer);
    let v = response.unwrap().json();
    let session = v["sessionId"].as_str().unwrap().to_owned();
    let mut s = tls_from(port, None).await;
    for (file, token) in v["files"].as_object().unwrap() {
        let r = exchange(
            &mut s,
            &upload_head(&session, file, token.as_str().unwrap(), 3),
            b"abc",
        )
        .await
        .unwrap();
        assert_eq!(r.status, 200);
    }
    let (outcome, saved) = b.finished(b.incoming().await).await;
    assert_eq!(outcome, Outcome::Done);
    assert_eq!(saved.len(), names.len() + 1);
    for name in &saved {
        assert!(is_safe(name), "{name:?}");
        assert!(!name.contains('/') && !name.contains('\\') && !name.starts_with('.'));
        assert_eq!(std::fs::read(b.downloads().join(name)).unwrap(), b"abc");
    }
    // Nothing anywhere else: the node's directory holds only its own two.
    let mut top: Vec<_> = std::fs::read_dir(b.dir.path())
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    top.sort();
    assert_eq!(top, ["data", "dl"]);
    assert_eq!(b.saved().len(), saved.len());
    // Nothing the UI got carries the bidi override or the zero-width space.
    let events = b.events_json();
    assert!(!events.contains('\u{202E}') && !events.contains('\u{200B}'));
    an_honest_transfer_still_works(&b).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn impossible_sizes_and_counts_never_reach_the_user() {
    let b = receiver();
    let port = b.receive().await;
    let over = MAX_FILE_BYTES + 1;
    let many: serde_json::Map<String, serde_json::Value> = (0..=MAX_FILES_PER_OFFER)
        .map(|i| (format!("{i}"), file_json(&format!("{i}"), "a", json!(1))))
        .collect();
    let cases: Vec<(&str, serde_json::Value)> = vec![
        ("negative", json!({"f": file_json("f", "a", json!(-1))})),
        (
            "i64::MIN",
            json!({"f": file_json("f", "a", json!(i64::MIN))}),
        ),
        (
            "u64::MAX",
            json!({"f": file_json("f", "a", json!(u64::MAX))}),
        ),
        ("over 8 GiB", json!({"f": file_json("f", "a", json!(over))})),
        (
            "over 16 GiB in all",
            json!({
                "a": file_json("a", "a", json!(MAX_FILE_BYTES)),
                "b": file_json("b", "b", json!(MAX_FILE_BYTES)),
                "c": file_json("c", "c", json!(1)),
            }),
        ),
        ("501 files", serde_json::Value::Object(many)),
        ("a string", json!({"f": file_json("f", "a", json!("12"))})),
        ("a float", json!({"f": file_json("f", "a", json!(1.5))})),
        ("no files", json!({})),
        (
            "a bad digest",
            json!({"f": {"id": "f", "fileName": "a", "size": 1, "fileType": "x", "sha256": "zz"}}),
        ),
    ];
    for (i, (what, files)) in cases.iter().enumerate() {
        let body = offer_json(EVE, "Eve", files);
        assert!(
            body.len() <= 64 * 1024,
            "{what} must fit the JSON cap to test the rule"
        );
        let r = post_from(port, src(10 + i as u8), PREPARE, &body)
            .await
            .unwrap();
        assert_eq!(r.status, 400, "{what}");
    }
    assert_eq!(b.offers_shown(), 0, "none of these reached the user");
    assert!(b.saved().is_empty());
    an_honest_transfer_still_works(&b).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn oversized_json_is_refused_before_it_is_read() {
    let b = receiver();
    let port = b.receive().await;
    let big = vec![b' '; 70 * 1024];

    // Declared too large: refused on the header, before a byte of body.
    let mut s = tls_from(port, src(20)).await;
    let r = exchange(&mut s, &post(PREPARE, 10_000_000_000), b"")
        .await
        .unwrap();
    assert_eq!(r.status, 413);

    // Sent too large. The server answers 413 and closes without reading
    // the rest, so the kernel may reset the connection while the body is
    // still arriving and take the 413 with it: under load the client sees
    // either. Both are the rule holding -- refused before it was read --
    // and `an_honest_transfer_still_works` below proves the server is fine.
    for (i, path) in [(21, PREPARE), (22, REGISTER)] {
        if let Some(r) = post_from(port, src(i), path, &big).await {
            assert_eq!(r.status, 413, "{path}");
        }
    }

    // Streamed without a length: cut off at the cap.
    let mut s = tls_from(port, src(23)).await;
    s.write_all(chunked_head(PREPARE).as_bytes()).await.unwrap();
    let mut refused = None;
    for _ in 0..100 {
        if s.write_all(&chunk(&[b' '; 4096])).await.is_err() {
            break;
        }
        if let Ok(Some(r)) =
            tokio::time::timeout(Duration::from_millis(5), read_response(&mut s)).await
        {
            refused = Some(r.status);
            break;
        }
    }
    let status = match refused {
        Some(s) => s,
        None => read_response(&mut s).await.map_or(0, |r| r.status),
    };
    assert!(status == 413 || status == 0, "{status}");
    assert_eq!(b.offers_shown(), 0);
    an_honest_transfer_still_works(&b).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn upload_bodies_are_held_to_the_declared_size() {
    let b = receiver();
    let port = b.receive().await;
    let files = json!({
        "longer": file_json("longer", "longer.bin", json!(10)),
        "chunked": file_json("chunked", "chunked.bin", json!(10)),
        "shorter": file_json("shorter", "shorter.bin", json!(10)),
    });
    let (session, tokens) = accepted(&b, port, None, &files).await;

    // Declares more than the offer did: refused before a staging file.
    let mut s = tls_from(port, None).await;
    let r = exchange(
        &mut s,
        &upload_head(&session, "longer", &tokens["longer"], 20),
        &[1; 20],
    )
    .await
    .unwrap();
    assert_ne!(r.status, 200);

    // Sends more than the offer declared, chunked: cut off at the size.
    let mut s = tls_from(port, None).await;
    let path = upload_path(&session, "chunked", &tokens["chunked"]);
    s.write_all(chunked_head(&path).as_bytes()).await.unwrap();
    let _ = s.write_all(&chunk(&[2; 8])).await;
    let _ = s.write_all(&chunk(&[2; 8])).await;
    let _ = s.write_all(b"0\r\n\r\n").await;
    assert_ne!(read_response(&mut s).await.map_or(0, |r| r.status), 200);

    // Sends less and hangs up.
    let mut s = tls_from(port, None).await;
    s.write_all(upload_head(&session, "shorter", &tokens["shorter"], 10).as_bytes())
        .await
        .unwrap();
    s.write_all(&[3; 5]).await.unwrap();
    s.flush().await.unwrap();
    drop(s);

    let (outcome, saved) = b.finished(b.incoming().await).await;
    assert!(matches!(outcome, Outcome::Failed { .. }), "{outcome:?}");
    assert!(saved.is_empty());
    assert!(b.saved().is_empty());
    assert_eq!(b.partials(), 0);
    an_honest_transfer_still_works(&b).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_stalled_upload_times_out_and_is_cleaned_up() {
    let b = receiver();
    let port = b.receive().await;
    let files = json!({"f": file_json("f", "slow.bin", json!(1000))});
    let (session, tokens) = accepted(&b, port, None, &files).await;
    let mut s = tls_from(port, None).await;
    s.write_all(upload_head(&session, "f", &tokens["f"], 1000).as_bytes())
        .await
        .unwrap();
    s.write_all(&[7; 500]).await.unwrap();
    s.flush().await.unwrap();
    // And now nothing, with the connection held open (slow-loris).
    let start = Instant::now();
    let (outcome, _) = b.finished(b.incoming().await).await;
    assert!(matches!(outcome, Outcome::Failed { error } if error.code == ErrorCode::Network));
    assert!(start.elapsed() < Duration::from_secs(10));
    assert!(b.saved().is_empty());
    assert_eq!(b.partials(), 0);
    an_honest_transfer_still_works(&b).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn slow_handshakes_heads_and_bodies_are_cut_off() {
    let b = receiver();
    let port = b.receive().await;

    // Connects and never says hello.
    let mut silent = tcp(port, src(30)).await;
    assert!(closed_within(&mut silent, Duration::from_secs(5)).await);

    // Half a request head, then nothing.
    let mut s = tls_from(port, src(31)).await;
    s.write_all(b"POST /api/localsend/v2/prepare-upload HTTP/1.1\r\nHost: x\r\n")
        .await
        .unwrap();
    s.flush().await.unwrap();
    assert!(closed_within(&mut s, Duration::from_secs(5)).await);

    // A JSON body dripped slower than the deadline allows.
    let mut s = tls_from(port, src(32)).await;
    s.write_all(post(PREPARE, 100).as_bytes()).await.unwrap();
    let mut answer = None;
    for _ in 0..20 {
        let _ = s.write_all(b" ").await;
        let _ = s.flush().await;
        if let Ok(r) = tokio::time::timeout(Duration::from_millis(400), read_response(&mut s)).await
        {
            answer = r;
            break;
        }
    }
    assert_eq!(answer.map(|r| r.status), Some(408));

    // An idle keep-alive connection is closed too.
    let mut s = tls_from(port, src(33)).await;
    let r = exchange(
        &mut s,
        "GET /api/localsend/v2/info HTTP/1.1\r\nHost: x\r\n\r\n",
        b"",
    )
    .await
    .unwrap();
    assert_eq!(r.status, 200);
    assert_eq!(r.json()["fingerprint"], identity(1).fingerprint);
    assert!(closed_within(&mut s, Duration::from_secs(5)).await);
    assert_eq!(b.offers_shown(), 0);
    an_honest_transfer_still_works(&b).await;
}

/// A peer that pipelines requests for 404s from `source` and never reads an
/// answer, with a receive buffer as small as the kernel allows: the
/// server's answers back up until it cannot write. The task holds the
/// connection until it is aborted.
async fn stops_reading(port: u16, source: Option<Ipv4Addr>) -> tokio::task::JoinHandle<()> {
    let socket = tokio::net::TcpSocket::new_v4().unwrap();
    socket.set_recv_buffer_size(1024).unwrap();
    socket
        .bind(SocketAddr::new(IpAddr::V4(source.unwrap()), 0))
        .unwrap();
    let tcp = socket
        .connect(SocketAddr::new(Ipv4Addr::LOCALHOST.into(), port))
        .await
        .unwrap();
    let mut s = tokio_rustls::TlsConnector::from(raw_client_config(Some(EVE)))
        .connect(
            rustls::pki_types::ServerName::IpAddress(Ipv4Addr::LOCALHOST.into()),
            tcp,
        )
        .await
        .unwrap();
    tokio::spawn(async move {
        // About 700 KiB of answers: several times every buffer between --
        // the server's capped send buffer, rustls's 64 KiB, hyper's own.
        let requests = b"GET /x HTTP/1.1\r\nHost: x\r\n\r\n".repeat(4000);
        let _ = s.write_all(&requests).await;
        std::future::pending::<()>().await;
    })
}

/// The server-side twin of `a_receiver_that_stops_reading_times_out`.
/// Nothing timed out a response that could not be written: four addresses
/// with eight such connections each held every connection the server has,
/// for as long as they liked, and every honest peer was turned away.
///
/// The receiver's handshake timeout is the longest there is, so neither
/// hyper's header timer nor a connection's lifetime (see
/// `a_connection_kept_busy_is_retired`) can end these connections within
/// the bound checked: only the write deadline can.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_sender_that_stops_reading_is_cut_off() {
    let mut cfg = NodeConfig::new(1, "Bob");
    cfg.idle = Duration::from_secs(6);
    cfg.handshake = Duration::from_secs(20);
    let b = Node::new(&cfg);
    let port = b.receive().await;
    let listening = b.ls.tasks_running();
    let start = Instant::now();
    let mut held = Vec::new();
    for a in 0..4 {
        for _ in 0..8 {
            held.push(stops_reading(port, src(90 + a)).await);
        }
    }
    // Every connection the server has is held: a fifth address is dropped
    // before its handshake.
    assert!(
        raw_tls(port, Some(EVE), src(95)).await.is_err(),
        "every connection is held"
    );
    // Every one ends once its answers have not moved for the idle timeout
    // -- well before the 26 s a connection may live.
    wait("the stalled connections to be cut off", || {
        (b.ls.tasks_running() <= listening).then_some(())
    })
    .await;
    assert!(
        start.elapsed() < Duration::from_secs(16),
        "cut off after {:?}",
        start.elapsed()
    );
    an_honest_transfer_still_works(&b).await;
    for h in held {
        h.abort();
    }
}

/// hyper's header timer is armed for each request anew, so a request just
/// before every timeout kept a connection -- and its slot -- for ever.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_connection_kept_busy_is_retired() {
    let b = receiver();
    let port = b.receive().await;
    let mut s = tls_from(port, src(94)).await;
    let start = Instant::now();
    let closed = loop {
        match exchange(&mut s, "GET /x HTTP/1.1\r\nHost: x\r\n\r\n", b"").await {
            None => break true,
            Some(r) => assert_eq!(r.status, 404),
        }
        if start.elapsed() > Duration::from_secs(15) {
            break false;
        }
        // Well within the 2 s header timeout.
        tokio::time::sleep(Duration::from_secs(1)).await;
    };
    assert!(closed, "a busy connection lives only so long");
    // The handshake and idle timeouts, and the grace to finish an answer.
    assert!(start.elapsed() < Duration::from_secs(12));
    an_honest_transfer_still_works(&b).await;
}

/// A registration from `source` as pool identity `id`, whose server is at
/// `port`.
async fn register_as(
    to: u16,
    id: usize,
    source: Option<Ipv4Addr>,
    port: u16,
) -> Option<RawResponse> {
    let body = serde_json::to_vec(&json!({
        "alias": format!("Eve{id}"), "version": "2.2", "deviceType": "mobile",
        "fingerprint": identity(id).fingerprint, "port": port, "protocol": "https"
    }))
    .unwrap();
    let mut s = raw_tls(to, Some(id), source).await.ok()?;
    exchange(&mut s, &post(REGISTER, body.len()), &body).await
}

fn found(b: &Node) -> Vec<String> {
    b.events
        .lock()
        .unwrap()
        .iter()
        .filter_map(|e| match e {
            Event::PeerFound { peer } => Some(peer.id.clone()),
            _ => None,
        })
        .collect()
}

/// clove's `one_destination_cannot_monopolise_the_peer_table`, for
/// LocalSend: the peer list once filled first come, first served, so one
/// address with certificates enough kept every later device off it.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn one_address_cannot_fill_the_peer_list() {
    let b = receiver();
    let port = b.receive().await;
    b.ls.start_discovery().await.unwrap();
    // Every certificate one address has: one over its share.
    for id in [0, 2, 3, 4, 5] {
        assert_eq!(
            register_as(port, id, src(96), 4000).await.unwrap().status,
            200
        );
    }
    let first = format!("ls:{}", identity(0).fingerprint);
    b.wait_event("the stalest of them to make way", |e| match e {
        Event::PeerLost { peer } if *peer == first => Some(()),
        _ => None,
    })
    .await;
    assert_eq!(found(&b).len(), 5);
    // Another address is listed as ever.
    assert_eq!(
        register_as(port, 0, src(97), 4000).await.unwrap().status,
        200
    );
    wait("a peer at another address listed", || {
        (found(&b).len() == 6).then_some(())
    })
    .await;
    an_honest_transfer_still_works(&b).await;
}

/// A TLS server with pool identity `id` that counts connections, and
/// requests that got past the handshake.
async fn counting_server(id: usize) -> (u16, Arc<AtomicUsize>, Arc<AtomicUsize>) {
    use rustls::pki_types::pem::PemObject;
    use rustls::pki_types::{CertificateDer, PrivateKeyDer};
    let id = identity(id);
    let config = rustls::ServerConfig::builder_with_provider(Arc::new(
        rustls::crypto::ring::default_provider(),
    ))
    .with_safe_default_protocol_versions()
    .unwrap()
    .with_no_client_auth()
    .with_single_cert(
        vec![CertificateDer::from_pem_slice(id.certificate_pem.as_bytes()).unwrap()],
        PrivateKeyDer::from_pem_slice(id.private_key_pem.as_bytes()).unwrap(),
    )
    .unwrap();
    let acceptor = tokio_rustls::TlsAcceptor::from(Arc::new(config));
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
        .await
        .unwrap();
    let port = listener.local_addr().unwrap().port();
    let connections = Arc::new(AtomicUsize::new(0));
    let requests = Arc::new(AtomicUsize::new(0));
    let (c, r) = (connections.clone(), requests.clone());
    tokio::spawn(async move {
        while let Ok((tcp, _)) = listener.accept().await {
            c.fetch_add(1, Ordering::SeqCst);
            let acceptor = acceptor.clone();
            let r = r.clone();
            tokio::spawn(async move {
                let Ok(mut s) = acceptor.accept(tcp).await else {
                    return;
                };
                let mut byte = [0u8; 1];
                if s.read(&mut byte).await.unwrap_or(0) > 0 {
                    r.fetch_add(1, Ordering::SeqCst);
                }
            });
        }
    });
    (port, connections, requests)
}

/// F-LS3 for the HTTP register fallback: a lead is registered with only
/// under the certificate it was known by. Here a peer registers as one
/// identity and names, as its server, a port where another certificate
/// answers.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_fallback_sends_nothing_to_another_certificate() {
    let mut cfg = NodeConfig::new(0, "Alice");
    cfg.rounds = Duration::from_secs(1);
    let a = Node::new(&cfg);
    let port = a.receive().await;
    let (other, connections, requests) = counting_server(2).await;
    assert_eq!(register_as(port, 4, None, other).await.unwrap().status, 200);
    a.ls.start_discovery().await.unwrap();
    wait("the fallback to try the lead", || {
        (connections.load(Ordering::SeqCst) > 0).then_some(())
    })
    .await;
    // Rounds go by; the lead was dropped at the mismatch, not retried.
    tokio::time::sleep(Duration::from_millis(3500)).await;
    assert_eq!(connections.load(Ordering::SeqCst), 1);
    assert_eq!(requests.load(Ordering::SeqCst), 0, "not a byte of HTTP");
    assert!(found(&a).is_empty());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn uploads_without_consent_or_the_right_token_are_refused_unread() {
    let b = receiver();
    let port = b.receive().await;

    // No session at all.
    let r = post_from(port, None, &upload_path("s", "f", "t"), &[0; 10])
        .await
        .unwrap();
    assert_eq!(r.status, 403);

    // A session pending the user's answer is not one to upload to.
    let files = json!({"f": file_json("f", "a.bin", json!(10))});
    let body = offer_json(EVE, "Eve", &files);
    let pending = tokio::spawn(async move { post_from(port, None, PREPARE, &body).await });
    let (offer, _) = b.offer().await;
    let r = post_from(port, None, &upload_path("guess", "f", "guess"), &[0; 10])
        .await
        .unwrap();
    assert_eq!(r.status, 403);
    assert_eq!(b.partials(), 0);
    b.answer(offer, true);
    let v = pending.await.unwrap().unwrap().json();
    let session = v["sessionId"].as_str().unwrap().to_owned();
    let token = v["files"]["f"].as_str().unwrap().to_owned();

    // Wrong token, wrong session, wrong file, and the right ones from
    // another address: all refused, nothing staged.
    for path in [
        upload_path(&session, "f", "0000"),
        upload_path("0000", "f", &token),
        upload_path(&session, "nope", &token),
    ] {
        let r = post_from(port, None, &path, &[0; 10]).await.unwrap();
        assert_eq!(r.status, 403, "{path}");
    }
    let r = post_from(port, src(40), &upload_path(&session, "f", &token), &[0; 10])
        .await
        .unwrap();
    assert_eq!(r.status, 403, "tokens are bound to the sender's address");
    // Same address, another certificate: a host spoofing the sender.
    let mut other = raw_tls(port, Some(2), None).await.unwrap();
    let r = exchange(
        &mut other,
        &post(&upload_path(&session, "f", &token), 10),
        &[0; 10],
    )
    .await
    .unwrap();
    assert_eq!(r.status, 403, "and to the sender's certificate");
    let mut other = raw_tls(port, Some(2), None).await.unwrap();
    let cancel = format!("/api/localsend/v2/cancel?sessionId={session}");
    let r = exchange(&mut other, &post(&cancel, 0), b"").await.unwrap();
    assert_eq!(r.status, 200, "answered, but");
    assert_eq!(b.partials(), 0);

    // The real sender still can.
    let r = post_from(port, None, &upload_path(&session, "f", &token), &[9; 10])
        .await
        .unwrap();
    assert_eq!(r.status, 200);
    // And not twice.
    let r = post_from(port, None, &upload_path(&session, "f", &token), &[9; 10])
        .await
        .unwrap();
    assert_eq!(r.status, 403);
    let (outcome, saved) = b.finished(b.incoming().await).await;
    assert_eq!(outcome, Outcome::Done);
    assert_eq!(saved, ["a.bin"]);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn offers_are_rate_limited_per_address() {
    let b = receiver();
    let port = b.receive().await;
    let body = offer_json(EVE, "Eve", &json!({}));
    for _ in 0..3 {
        assert_eq!(
            post_from(port, src(50), PREPARE, &body)
                .await
                .unwrap()
                .status,
            400
        );
    }
    assert_eq!(
        post_from(port, src(50), PREPARE, &body)
            .await
            .unwrap()
            .status,
        429
    );
    assert_eq!(
        post_from(port, src(51), PREPARE, &body)
            .await
            .unwrap()
            .status,
        400
    );
    an_honest_transfer_still_works(&b).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_pin_gates_the_offer_and_consent_still_follows() {
    let mut cfg = NodeConfig::new(1, "Bob");
    cfg.pin = Some("4321".into());
    let b = Node::new(&cfg);
    let port = b.receive().await;
    let files = json!({"f": file_json("f", "a.bin", json!(1))});
    let body = offer_json(EVE, "Eve", &files);
    let r = post_from(port, src(60), PREPARE, &body).await.unwrap();
    assert_eq!(r.status, 401);
    let r = post_from(port, src(60), &format!("{PREPARE}?pin=1234"), &body)
        .await
        .unwrap();
    assert_eq!(r.status, 401);
    assert_eq!(b.offers_shown(), 0);
    // The right PIN only gets the offer in front of the user (F-C2).
    let answer = async {
        let (id, _) = b.offer().await;
        b.answer(id, false);
    };
    let with_pin = format!("{PREPARE}?pin=4321");
    let (r, ()) = tokio::join!(post_from(port, src(61), &with_pin, &body), answer);
    assert_eq!(r.unwrap().status, 403);
    assert!(b.saved().is_empty());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn registrations_must_prove_their_fingerprint() {
    let b = receiver();
    let port = b.receive().await;
    b.ls.start_discovery().await.unwrap();
    let reg = |alias: &str, fingerprint: &str, protocol: &str| {
        serde_json::to_vec(&json!({
            "alias": alias, "version": "2.2", "deviceModel": "M\u{202E}odel", "deviceType": "mobile",
            "fingerprint": fingerprint, "port": 4000, "protocol": protocol
        }))
        .unwrap()
    };
    // Claims someone else's fingerprint.
    let r = post_from(
        port,
        src(70),
        REGISTER,
        &reg("Eve", &identity(2).fingerprint, "https"),
    )
    .await
    .unwrap();
    assert_eq!(r.status, 403);
    // Says it speaks plain HTTP (F-LS2).
    let r = post_from(
        port,
        src(71),
        REGISTER,
        &reg("Eve", &identity(EVE).fingerprint, "http"),
    )
    .await
    .unwrap();
    assert_eq!(r.status, 403);
    // Junk.
    let r = post_from(port, src(72), REGISTER, b"{\"alias\":")
        .await
        .unwrap();
    assert_eq!(r.status, 400);
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(!b.events_json().contains("peer_found"));

    // Honest, with a spoofing alias: listed, sanitised.
    let r = post_from(
        port,
        src(73),
        REGISTER,
        &reg(
            "\u{202E}Eve\u{200B}",
            &identity(EVE).fingerprint.to_lowercase(),
            "https",
        ),
    )
    .await
    .unwrap();
    assert_eq!(r.status, 200);
    assert_eq!(r.json()["fingerprint"], identity(1).fingerprint);
    assert_eq!(r.json()["alias"], "Bob");
    let peer = b
        .wait_event("Eve listed", |e| match e {
            Event::PeerFound { peer } => Some(peer.clone()),
            _ => None,
        })
        .await;
    assert_eq!(peer.name, "Eve");
    assert_eq!(peer.model.as_deref(), Some("Model"));
    assert_eq!(peer.id, format!("ls:{}", identity(EVE).fingerprint));
}

/// A deterministic mutation sweep in the manner of clove's `hostile.rs`:
/// one valid request -- head and body -- corrupted a few hundred ways, each
/// sent on its own connection. Whatever arrives, the receiver keeps its
/// rules: no panic, nothing written, every offer the user sees sanitised,
/// and an honest peer still gets through afterwards.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_mutation_sweep_of_offers_breaks_nothing() {
    let b = receiver();
    let port = b.receive().await;
    let files = json!({
        "f1": {"id": "f1", "fileName": "../../a.txt", "size": 5, "fileType": "text/plain", "sha256": "ab".repeat(32)},
        "f2": {"id": "f2", "fileName": "b\u{202E}.exe", "size": 0, "fileType": "application/x", "preview": "p"},
    });
    let body = offer_json(EVE, "Eve", &files);
    let mut request = post(PREPARE, body.len()).into_bytes();
    request.extend_from_slice(&body);

    // Answer every offer that gets through with a no.
    let decliner = {
        let ctx = b.ctx.clone();
        let consent = b.consent.clone();
        tokio::spawn(async move {
            let mut answered = std::collections::HashSet::new();
            loop {
                let pending: Vec<_> = consent
                    .lock()
                    .unwrap()
                    .iter()
                    .filter_map(|e| match e {
                        sukkula_core::consent::ConsentEvent::Pending { id, offer } => {
                            Some((*id, offer.clone()))
                        }
                        sukkula_core::consent::ConsentEvent::Closed { .. } => None,
                    })
                    .collect();
                for (id, _) in pending {
                    if answered.insert(id) {
                        ctx.consent()
                            .answer(id, sukkula_core::consent::Decision::Decline);
                    }
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
    };

    let mut x: u64 = 0x9E37_79B9_7F4A_7C15;
    let mut next = move || {
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        x
    };
    let mut statuses: HashMap<u16, usize> = HashMap::new();
    for i in 0..240u32 {
        let mut m = request.clone();
        match next() % 4 {
            0 => {
                // Flip a few bytes.
                for _ in 0..1 + next() % 4 {
                    let at = (next() as usize) % m.len();
                    m[at] ^= 1 << (next() % 8);
                }
            }
            1 => {
                // Cut it short.
                let at = (next() as usize) % m.len();
                m.truncate(at);
            }
            2 => {
                // Insert junk.
                let at = (next() as usize) % m.len();
                let junk: Vec<u8> = (0..1 + next() % 16).map(|_| next() as u8).collect();
                m.splice(at..at, junk);
            }
            _ => {
                // Overwrite a byte with a JSON or HTTP metacharacter.
                const META: &[u8] = b"\"{}[],:\\\r\n\0 -9";
                let at = (next() as usize) % m.len();
                m[at] = META[(next() as usize) % META.len()];
            }
        }
        // A fresh address each time, so the offer limit is not what answers.
        let source = Ipv4Addr::new(127, 1, (i / 200) as u8, (i % 200) as u8 + 1);
        let Ok(mut s) = raw_tls(port, Some(EVE), Some(source)).await else {
            continue;
        };
        let _ = s.write_all(&m).await;
        // Half-close, so a mutation that promises more body than it has is
        // answered at once instead of at the deadline.
        let _ = s.shutdown().await;
        let status = tokio::time::timeout(Duration::from_secs(5), read_response(&mut s))
            .await
            .ok()
            .flatten()
            .map_or(0, |r| r.status);
        *statuses.entry(status).or_default() += 1;
    }
    decliner.abort();
    assert!(
        !statuses.contains_key(&200),
        "no mutation became a session: {statuses:?}"
    );
    // Whatever reached the user was sanitised first.
    for e in b.consent.lock().unwrap().iter() {
        if let sukkula_core::consent::ConsentEvent::Pending { offer, .. } = e {
            assert!(offer.files.iter().all(|f| is_safe(f.name.as_str())));
            assert!(!offer.sender.contains('\u{202E}'));
        }
    }
    assert!(b.saved().is_empty());
    assert_eq!(b.partials(), 0);
    let events = b.events_json();
    assert!(!events.contains('\u{202E}'));
    an_honest_transfer_still_works(&b).await;
}

// ------------------------------------------------------- hostile receivers

/// How the hostile receiver misbehaves once asked to take files.
#[derive(Clone, Copy, Debug)]
enum Trick {
    /// Answers the offer with an endless body.
    EndlessAnswer,
    /// Accepts, then stops reading the upload.
    StopReading,
    /// Never answers the offer.
    Silent,
}

/// A TLS server with identity 4 that answers `GET /info` properly and then
/// plays `trick`.
async fn hostile_receiver(trick: Trick) -> u16 {
    use rustls::pki_types::pem::PemObject;
    use rustls::pki_types::{CertificateDer, PrivateKeyDer};
    let id = identity(4);
    let config = rustls::ServerConfig::builder_with_provider(Arc::new(
        rustls::crypto::ring::default_provider(),
    ))
    .with_safe_default_protocol_versions()
    .unwrap()
    .with_no_client_auth()
    .with_single_cert(
        vec![CertificateDer::from_pem_slice(id.certificate_pem.as_bytes()).unwrap()],
        PrivateKeyDer::from_pem_slice(id.private_key_pem.as_bytes()).unwrap(),
    )
    .unwrap();
    let acceptor = tokio_rustls::TlsAcceptor::from(Arc::new(config));
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
        .await
        .unwrap();
    let port = listener.local_addr().unwrap().port();
    let info = serde_json::to_vec(&json!({
        "alias": "Receiver\u{202E}", "version": "2.2", "fingerprint": id.fingerprint, "download": false
    }))
    .unwrap();
    tokio::spawn(async move {
        loop {
            let Ok((tcp, _)) = listener.accept().await else {
                return;
            };
            let acceptor = acceptor.clone();
            let info = info.clone();
            tokio::spawn(async move {
                let Ok(mut s) = acceptor.accept(tcp).await else {
                    return;
                };
                let mut head = Vec::new();
                let mut byte = [0u8; 1];
                while !head.ends_with(b"\r\n\r\n") {
                    if s.read(&mut byte).await.unwrap_or(0) == 0 {
                        return;
                    }
                    head.push(byte[0]);
                }
                let head = String::from_utf8_lossy(&head).into_owned();
                if head.starts_with("GET /api/localsend/v2/info") {
                    let r = format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                        info.len()
                    );
                    let _ = s.write_all(r.as_bytes()).await;
                    let _ = s.write_all(&info).await;
                    let _ = s.shutdown().await;
                    return;
                }
                match trick {
                    Trick::EndlessAnswer => {
                        let _ = s
                            .write_all(b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: 100000000000\r\n\r\n{\"sessionId\":\"")
                            .await;
                        let junk = vec![b'a'; 64 * 1024];
                        while s.write_all(&junk).await.is_ok() {}
                    }
                    Trick::StopReading => {
                        if head.contains("prepare-upload") {
                            let answer = br#"{"sessionId":"s1","files":{"0":"t0"}}"#;
                            let r = format!(
                                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n",
                                answer.len()
                            );
                            // Drain the offer body first.
                            let mut sink = vec![0u8; 64 * 1024];
                            let _ =
                                tokio::time::timeout(Duration::from_millis(200), s.read(&mut sink))
                                    .await;
                            let _ = s.write_all(r.as_bytes()).await;
                            let _ = s.write_all(answer).await;
                            // Then the upload's head arrives on the same
                            // connection, and nothing more is read.
                        }
                        tokio::time::sleep(Duration::from_secs(30)).await;
                    }
                    Trick::Silent => tokio::time::sleep(Duration::from_secs(30)).await,
                }
            });
        }
    });
    port
}

async fn send_to_hostile(trick: Trick, file_len: u64) -> (Outcome, Duration) {
    let mut cfg = NodeConfig::new(0, "Alice");
    cfg.idle = Duration::from_secs(2);
    cfg.handshake = Duration::from_secs(2);
    cfg.prepare = Duration::from_secs(2);
    let a = Node::new(&cfg);
    let port = hostile_receiver(trick).await;
    a.ls.start_discovery().await.unwrap();
    let peer =
        a.ls.discover_at(std::net::SocketAddr::new(Ipv4Addr::LOCALHOST.into(), port))
            .await
            .unwrap();
    let dir = tempfile::tempdir().unwrap();
    let f = sparse_file(dir.path(), "f.bin", file_len);
    let start = Instant::now();
    let id =
        a.ls.send(SendTarget::LocalSend { peer }, vec![outgoing(&f)])
            .await
            .unwrap();
    let outcome = a.finished(id).await.0;
    // The UI never saw the receiver's bidi override.
    assert!(!a.events_json().contains('\u{202E}'));
    (outcome, start.elapsed())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_receiver_answering_without_end_is_cut_off_at_the_cap() {
    let (outcome, took) = send_to_hostile(Trick::EndlessAnswer, 10).await;
    assert!(
        matches!(&outcome, Outcome::Failed { error } if error.code == ErrorCode::Network),
        "{outcome:?}"
    );
    assert!(took < Duration::from_secs(10));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_receiver_that_stops_reading_times_out() {
    // Larger than loopback's socket buffers, so the feed itself stalls.
    let (outcome, took) = send_to_hostile(Trick::StopReading, 64 * 1024 * 1024).await;
    assert!(
        matches!(&outcome, Outcome::Failed { error } if error.code == ErrorCode::Network),
        "{outcome:?}"
    );
    assert!(took < Duration::from_secs(10));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_receiver_that_never_answers_times_out() {
    let (outcome, took) = send_to_hostile(Trick::Silent, 10).await;
    assert!(
        matches!(&outcome, Outcome::Failed { error } if error.code == ErrorCode::Network),
        "{outcome:?}"
    );
    assert!(took < Duration::from_secs(10));
}

// ---------------------------------------------------------------- draining

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn stopping_receiving_lets_the_running_transfer_finish_and_nothing_else_in() {
    // A generous idle timeout: the upload pauses while receiving stops.
    let mut cfg = NodeConfig::new(1, "Bob");
    cfg.idle = Duration::from_secs(10);
    let b = Node::new(&cfg);
    let port = b.receive().await;
    let files = json!({"f": file_json("f", "kept.bin", json!(1000))});
    let (session, tokens) = accepted(&b, port, None, &files).await;
    let mut s = tls_from(port, None).await;
    s.write_all(upload_head(&session, "f", &tokens["f"], 1000).as_bytes())
        .await
        .unwrap();
    s.write_all(&[5; 400]).await.unwrap();
    s.flush().await.unwrap();

    b.ls.stop_receiving().await;
    // New peers are turned away at once...
    assert!(raw_tls(port, Some(EVE), src(80)).await.is_err());
    // ...but the running upload finishes.
    s.write_all(&[5; 600]).await.unwrap();
    s.flush().await.unwrap();
    // The last response goes out before the listener's connections close.
    assert_eq!(read_response(&mut s).await.unwrap().status, 200);
    let (outcome, saved) = b.finished(b.incoming().await).await;
    assert_eq!(outcome, Outcome::Done);
    assert_eq!(saved, ["kept.bin"]);
    // And then the listener closes.
    wait("the listener to close", || {
        std::net::TcpStream::connect(("127.0.0.1", port))
            .is_err()
            .then_some(())
    })
    .await;
    assert_eq!(b.ls.port().await, None);
    // Receiving again works.
    b.receive().await;
    an_honest_transfer_still_works(&b).await;
}
