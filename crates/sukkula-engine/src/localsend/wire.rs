//! What goes over the wire, and the bounded ways of reading it.
//!
//! The JSON shapes are upstream's own DTOs, so the wire format is the
//! protocol authors' by construction. What is ours is how much of it is read
//! and for how long (S4, S6): every JSON body is at most
//! [`MAX_MESSAGE_BYTES`], read chunk by chunk with an idle timeout and an
//! overall deadline, before it is parsed at all.

use std::convert::Infallible;
use std::pin::Pin;
use std::task::{Context, Poll};
use std::time::Duration;

use bytes::Bytes;
use http_body_util::BodyExt;
use hyper::body::{Body, Frame, SizeHint};
use localsend::model::discovery::DeviceType as LsDeviceType;
use sukkula_core::hex;
use sukkula_core::limits::{MAX_ALIAS_CHARS, MAX_MESSAGE_BYTES, MAX_MODEL_CHARS};
use sukkula_core::text;
use tokio::sync::mpsc;
use tokio::time::Instant;

use crate::api::DeviceType;

/// The API path prefix of protocol v2.
pub(super) const API_V2: &str = "/api/localsend/v2";

/// The longest session id or file token accepted from a peer. Ours are 32
/// hex digits; LocalSend's are UUIDs.
pub(super) const MAX_TOKEN_BYTES: usize = 128;

/// Why a bounded read stopped.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ReadError {
    /// Over the cap.
    TooLarge,
    /// No progress within the idle timeout, or past the deadline.
    TimedOut,
    /// The connection failed.
    Broken,
}

/// Reads a whole body of at most `cap` bytes, allowing `idle` between
/// chunks and `total` for all of it. A declared length over the cap is
/// refused before a byte is read.
pub(super) async fn read_capped<B>(
    mut body: B,
    cap: usize,
    idle: Duration,
    total: Duration,
) -> Result<Vec<u8>, ReadError>
where
    B: Body<Data = Bytes> + Unpin,
{
    let cap64 = u64::try_from(cap).unwrap_or(u64::MAX);
    if body.size_hint().lower() > cap64 {
        return Err(ReadError::TooLarge);
    }
    let deadline = Instant::now()
        .checked_add(total)
        .unwrap_or_else(Instant::now);
    let mut out: Vec<u8> = Vec::new();
    loop {
        let wait = idle.min(deadline.saturating_duration_since(Instant::now()));
        let frame = match tokio::time::timeout(wait, body.frame()).await {
            Err(_) => return Err(ReadError::TimedOut),
            Ok(None) => return Ok(out),
            Ok(Some(Err(_))) => return Err(ReadError::Broken),
            Ok(Some(Ok(frame))) => frame,
        };
        let Ok(data) = frame.into_data() else {
            continue; // trailers carry nothing we read
        };
        if out.len().saturating_add(data.len()) > cap {
            return Err(ReadError::TooLarge);
        }
        out.extend_from_slice(&data);
    }
}

/// Reads and parses a JSON body of at most [`MAX_MESSAGE_BYTES`].
pub(super) async fn read_json<T, B>(
    body: B,
    idle: Duration,
    total: Duration,
) -> Result<T, JsonError>
where
    T: serde::de::DeserializeOwned,
    B: Body<Data = Bytes> + Unpin,
{
    let bytes = read_capped(body, MAX_MESSAGE_BYTES, idle, total)
        .await
        .map_err(JsonError::Read)?;
    serde_json::from_slice(&bytes).map_err(|_| JsonError::Malformed)
}

/// Why a JSON body could not be had.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum JsonError {
    /// Reading it failed.
    Read(ReadError),
    /// It is not the expected JSON.
    Malformed,
}

/// A fingerprint as LocalSend writes it -- 64 hex digits -- in the
/// uppercase form upstream computes. Anything else is not a fingerprint.
pub(super) fn fingerprint(raw: &str) -> Option<String> {
    hex::decode_32(raw.trim()).map(|_| raw.trim().to_ascii_uppercase())
}

/// The peer id the UI knows a LocalSend peer by.
pub(super) fn peer_id(fingerprint: &str) -> String {
    format!("ls:{fingerprint}")
}

/// A peer's alias for the UI (S2). Never empty.
pub(super) fn alias(raw: &str) -> String {
    let shown = text::display(raw, MAX_ALIAS_CHARS);
    if shown.is_empty() {
        sukkula_core::offer::UNKNOWN_SENDER.to_owned()
    } else {
        shown
    }
}

/// A peer's model for the UI (S2).
pub(super) fn model(raw: Option<&str>) -> Option<String> {
    raw.map(|m| text::display(m, MAX_MODEL_CHARS))
        .filter(|m| !m.is_empty())
}

/// What icon a LocalSend device type gets.
pub(super) fn device_type(raw: Option<&LsDeviceType>) -> DeviceType {
    match raw {
        Some(LsDeviceType::Mobile) => DeviceType::Phone,
        Some(LsDeviceType::Desktop) => DeviceType::Computer,
        Some(LsDeviceType::Web | LsDeviceType::Headless | LsDeviceType::Server) | None => {
            DeviceType::Unknown
        }
    }
}

/// A session id or token from a peer, if it is short printable ASCII that
/// can go into a query string without surprises.
pub(super) fn token(raw: &str) -> Option<&str> {
    let ok = !raw.is_empty()
        && raw.len() <= MAX_TOKEN_BYTES
        && raw
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"-_.~".contains(&b));
    ok.then_some(raw)
}

/// 128 random bits as hex: session ids and file tokens.
pub(super) fn random_token() -> Result<String, getrandom::Error> {
    let mut bytes = [0u8; 16];
    getrandom::fill(&mut bytes)?;
    Ok(hex::encode(&bytes))
}

/// Compares two secrets without an early exit.
pub(super) fn same_secret(a: &str, b: &str) -> bool {
    let (a, b) = (a.as_bytes(), b.as_bytes());
    if a.len() != b.len() {
        return false;
    }
    a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

/// The first value of `key` in a query string.
pub(super) fn query_value(query: Option<&str>, key: &str) -> Option<String> {
    form_urlencoded::parse(query?.as_bytes())
        .find(|(k, _)| k == key)
        .map(|(_, v)| v.into_owned())
}

/// `path?k=v&...`, percent-encoded.
pub(super) fn path_with_query(path: &str, params: &[(&str, &str)]) -> String {
    if params.is_empty() {
        return path.to_owned();
    }
    let query = form_urlencoded::Serializer::new(String::new())
        .extend_pairs(params)
        .finish();
    format!("{path}?{query}")
}

/// A request or response body we send: nothing, a buffer, or a stream of
/// chunks of an exactly known length (so hyper sends `Content-Length`, and
/// ends the connection if the stream stops short).
#[derive(Debug)]
pub(super) enum OutBody {
    /// Empty.
    Empty,
    /// One buffer, sent once.
    Full(Option<Bytes>),
    /// Chunks from a producer, `len` bytes in all.
    Stream {
        /// The chunks.
        rx: mpsc::Receiver<Bytes>,
        /// Their total.
        len: u64,
    },
}

impl OutBody {
    /// A JSON body.
    pub(super) fn json<T: serde::Serialize>(value: &T) -> OutBody {
        OutBody::Full(Some(Bytes::from(
            serde_json::to_vec(value).unwrap_or_default(),
        )))
    }
}

impl Body for OutBody {
    type Data = Bytes;
    type Error = Infallible;

    fn poll_frame(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Bytes>, Infallible>>> {
        match self.get_mut() {
            OutBody::Empty => Poll::Ready(None),
            OutBody::Full(b) => Poll::Ready(b.take().map(|b| Ok(Frame::data(b)))),
            OutBody::Stream { rx, .. } => rx.poll_recv(cx).map(|c| c.map(|b| Ok(Frame::data(b)))),
        }
    }

    fn is_end_stream(&self) -> bool {
        match self {
            OutBody::Empty => true,
            OutBody::Full(b) => b.is_none(),
            OutBody::Stream { .. } => false,
        }
    }

    fn size_hint(&self) -> SizeHint {
        match self {
            OutBody::Empty => SizeHint::with_exact(0),
            OutBody::Full(b) => SizeHint::with_exact(
                b.as_ref()
                    .map_or(0, |b| u64::try_from(b.len()).unwrap_or(0)),
            ),
            OutBody::Stream { len, .. } => SizeHint::with_exact(*len),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use http_body_util::Full;

    #[tokio::test]
    async fn capped_reads_refuse_what_is_too_large() {
        let ok = read_capped(
            Full::new(Bytes::from_static(b"{}")),
            10,
            Duration::from_secs(1),
            Duration::from_secs(1),
        )
        .await;
        assert_eq!(ok.unwrap(), b"{}");
        let big = read_capped(
            Full::new(Bytes::from(vec![b'x'; 11])),
            10,
            Duration::from_secs(1),
            Duration::from_secs(1),
        )
        .await;
        assert_eq!(big, Err(ReadError::TooLarge));
        // A stream without a length is cut off at the cap, not buffered.
        assert_eq!(
            read_capped(Endless, 10, Duration::from_secs(1), Duration::from_secs(1)).await,
            Err(ReadError::TooLarge)
        );
    }

    /// A body that never ends and declares no length.
    struct Endless;

    impl Body for Endless {
        type Data = Bytes;
        type Error = Infallible;
        fn poll_frame(
            self: Pin<&mut Self>,
            _: &mut Context<'_>,
        ) -> Poll<Option<Result<Frame<Bytes>, Infallible>>> {
            Poll::Ready(Some(Ok(Frame::data(Bytes::from_static(b"xxxx")))))
        }
    }

    #[test]
    fn fingerprints_and_tokens_are_strict() {
        let fp = "ab".repeat(32);
        assert_eq!(fingerprint(&fp), Some("AB".repeat(32)));
        assert_eq!(fingerprint("random string"), None);
        assert_eq!(fingerprint(&"a".repeat(65)), None);
        assert_eq!(token("abc-123"), Some("abc-123"));
        assert_eq!(token("a&b"), None);
        assert_eq!(token("a\r\nX: y"), None);
        assert_eq!(token(&"a".repeat(129)), None);
        assert!(same_secret("abc", "abc"));
        assert!(!same_secret("abc", "abd"));
        assert!(!same_secret("abc", "abcd"));
    }

    #[test]
    fn queries_round_trip_percent_encoded() {
        let p = path_with_query("/x", &[("sessionId", "a b&c"), ("pin", "12")]);
        assert_eq!(p, "/x?sessionId=a+b%26c&pin=12");
        let q = p.split_once('?').map(|(_, q)| q);
        assert_eq!(query_value(q, "sessionId").as_deref(), Some("a b&c"));
        assert_eq!(query_value(q, "pin").as_deref(), Some("12"));
        assert_eq!(query_value(q, "missing"), None);
        assert_eq!(query_value(None, "pin"), None);
    }
}
