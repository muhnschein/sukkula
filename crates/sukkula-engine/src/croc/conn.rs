//! croc's framing on one TCP connection: `"croc"`, a 32-bit little-endian
//! length, the payload (`src/comm/comm.go`).
//!
//! Each connection has a reader task of its own that turns the stream into
//! frames and hands them over one at a time, so that waiting for a frame
//! can be abandoned -- for a user's answer, a cancel, a timeout -- without
//! losing half of one. The task holds at most one frame while the one
//! before it waits to be taken: a peer that sends faster than the transfer
//! takes is slowed by TCP, not buffered. A frame longer than the
//! connection's limit, or without the magic, ends the connection.
//!
//! The relay sends a one-byte `0x01` every second to a client waiting for
//! its partner, unencrypted, on every connection: [`Conn::recv`] passes
//! them on, since whether they come says who was first; everything else
//! skips them with [`Conn::recv_skipping`].

use std::task::{Context, Poll};
use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::net::tcp::OwnedWriteHalf;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tokio::time::Instant;

use crate::api::{ErrorCode, ErrorInfo};

/// Every frame starts with this.
const MAGIC: &[u8; 4] = b"croc";

/// The relay's keepalive.
pub(super) const PING: &[u8] = &[1];

/// A connection, framed.
pub(super) struct Conn {
    write: OwnedWriteHalf,
    frames: mpsc::Receiver<Result<Vec<u8>, ErrorInfo>>,
    reader: JoinHandle<()>,
}

impl std::fmt::Debug for Conn {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Conn")
    }
}

impl Drop for Conn {
    fn drop(&mut self) {
        self.reader.abort();
    }
}

pub(super) fn network(what: &str) -> ErrorInfo {
    ErrorInfo::new(ErrorCode::Network, what)
}

impl Conn {
    /// Connects to `host:port`, trying each address it resolves to, within
    /// `limit`, reading frames of at most `max` bytes.
    ///
    /// # Errors
    ///
    /// [`ErrorCode::Network`]: no address answered.
    pub(super) async fn open(
        host: &str,
        port: u16,
        max: usize,
        limit: Duration,
    ) -> Result<Conn, ErrorInfo> {
        let connecting = async {
            let addrs = tokio::net::lookup_host((host, port))
                .await
                .map_err(|_| network("the croc relay's name does not resolve"))?;
            let mut last = network("the croc relay's name does not resolve");
            // Bounded: a name that resolves to a thousand addresses gets
            // the first few tried.
            for addr in addrs.take(8) {
                match TcpStream::connect(addr).await {
                    Ok(stream) => return Ok(stream),
                    Err(_) => last = network("the croc relay does not answer"),
                }
            }
            Err(last)
        };
        let stream = tokio::time::timeout(limit, connecting)
            .await
            .map_err(|_| network("the croc relay does not answer"))??;
        Ok(Conn::from_stream(stream, max))
    }

    /// Frames an accepted or connected stream.
    pub(super) fn from_stream(stream: TcpStream, max: usize) -> Conn {
        // Frames are written whole, one write each; Nagle would only hold
        // the last one of a burst back.
        let _ = stream.set_nodelay(true);
        let (mut read, write) = stream.into_split();
        let (tx, frames) = mpsc::channel(1);
        let reader = tokio::spawn(async move {
            loop {
                let frame = read_frame(&mut read, max).await;
                let end = frame.is_err();
                if tx.send(frame).await.is_err() || end {
                    return;
                }
            }
        });
        Conn {
            write,
            frames,
            reader,
        }
    }

    /// Sends one frame within `limit`.
    ///
    /// # Errors
    ///
    /// [`ErrorCode::Network`]: the write failed or stalled.
    pub(super) async fn send(&mut self, payload: &[u8], limit: Duration) -> Result<(), ErrorInfo> {
        let len = u32::try_from(payload.len()).map_err(|_| network("a frame too long to send"))?;
        let mut frame = Vec::with_capacity(payload.len().saturating_add(8));
        frame.extend_from_slice(MAGIC);
        frame.extend_from_slice(&len.to_le_bytes());
        frame.extend_from_slice(payload);
        tokio::time::timeout(limit, self.write.write_all(&frame))
            .await
            .map_err(|_| network("the croc relay stopped taking data"))?
            .map_err(|_| network("the croc connection broke"))
    }

    /// The next frame, keepalives included, within `limit`.
    ///
    /// # Errors
    ///
    /// [`ErrorCode::Network`]: the connection ended, broke the framing, or
    /// sent nothing in time.
    pub(super) async fn recv(&mut self, limit: Duration) -> Result<Vec<u8>, ErrorInfo> {
        match tokio::time::timeout(limit, self.frames.recv()).await {
            Ok(Some(frame)) => frame,
            Ok(None) => Err(network("the croc connection ended")),
            Err(_) => Err(network("the croc peer stopped responding")),
        }
    }

    /// The next frame if one comes within `limit`, `None` if none does.
    ///
    /// # Errors
    ///
    /// [`ErrorCode::Network`]: the connection ended or broke the framing.
    pub(super) async fn recv_within(
        &mut self,
        limit: Duration,
    ) -> Result<Option<Vec<u8>>, ErrorInfo> {
        match tokio::time::timeout(limit, self.frames.recv()).await {
            Ok(Some(frame)) => frame.map(Some),
            Ok(None) => Err(network("the croc connection ended")),
            Err(_) => Ok(None),
        }
    }

    /// The next frame that is not a keepalive, by `deadline`.
    ///
    /// # Errors
    ///
    /// As [`Conn::recv`].
    pub(super) async fn recv_skipping(&mut self, deadline: Instant) -> Result<Vec<u8>, ErrorInfo> {
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            let frame = self.recv(left).await?;
            if frame != PING {
                return Ok(frame);
            }
        }
    }

    /// The next frame, keepalives included, if one is there: what the
    /// receiver's merge of several data connections polls.
    pub(super) fn poll_frame(&mut self, cx: &mut Context<'_>) -> Poll<Result<Vec<u8>, ErrorInfo>> {
        self.frames
            .poll_recv(cx)
            .map(|f| f.unwrap_or_else(|| Err(network("the croc connection ended"))))
    }
}

/// One frame of at most `max` bytes from `read`.
pub(super) async fn read_frame<R: AsyncReadExt + Unpin>(
    read: &mut R,
    max: usize,
) -> Result<Vec<u8>, ErrorInfo> {
    let mut header = [0u8; 8];
    read.read_exact(&mut header)
        .await
        .map_err(|_| network("the croc connection ended"))?;
    let (magic, len) = header.split_at(4);
    if magic != MAGIC {
        return Err(network("not a croc frame"));
    }
    let len = u32::from_le_bytes(len.try_into().map_err(|_| network("not a croc frame"))?);
    let len = usize::try_from(len).map_err(|_| network("a croc frame too long"))?;
    if len > max {
        return Err(network("a croc frame too long"));
    }
    let mut body = vec![0u8; len];
    read.read_exact(&mut body)
        .await
        .map_err(|_| network("the croc connection ended"))?;
    Ok(body)
}

/// A frame as bytes on the wire.
#[cfg(test)]
pub(super) fn frame(payload: &[u8]) -> Vec<u8> {
    let mut out = MAGIC.to_vec();
    out.extend_from_slice(
        &u32::try_from(payload.len())
            .unwrap_or(u32::MAX)
            .to_le_bytes(),
    );
    out.extend_from_slice(payload);
    out
}

#[cfg(test)]
#[allow(clippy::arithmetic_side_effects, clippy::field_reassign_with_default)] // Test scenes.
mod tests {
    use super::*;
    use tokio::net::TcpListener;

    async fn pair(max: usize) -> (Conn, TcpStream) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let (conn, accepted) = tokio::join!(
            Conn::open("127.0.0.1", port, max, Duration::from_secs(5)),
            listener.accept()
        );
        (conn.unwrap(), accepted.unwrap().0)
    }

    #[tokio::test]
    async fn frames_go_both_ways_and_keepalives_are_skipped() {
        let (mut conn, mut peer) = pair(64).await;
        conn.send(b"hello", Duration::from_secs(1)).await.unwrap();
        let mut got = [0u8; 13];
        peer.read_exact(&mut got).await.unwrap();
        assert_eq!(&got, b"croc\x05\x00\x00\x00hello");
        peer.write_all(&frame(PING)).await.unwrap();
        peer.write_all(&frame(b"")).await.unwrap();
        peer.write_all(&frame(PING)).await.unwrap();
        peer.write_all(&frame(b"x")).await.unwrap();
        assert_eq!(conn.recv(Duration::from_secs(1)).await.unwrap(), PING);
        let deadline = Instant::now() + Duration::from_secs(1);
        assert_eq!(conn.recv_skipping(deadline).await.unwrap(), b"");
        assert_eq!(conn.recv_skipping(deadline).await.unwrap(), b"x");
    }

    #[tokio::test]
    async fn broken_framing_ends_the_connection() {
        for bad in [
            b"CROC\x01\x00\x00\x00x".to_vec(),
            frame(&[0u8; 65]),
            b"croc\xff\xff\xff\xff".to_vec(),
        ] {
            let (mut conn, mut peer) = pair(64).await;
            peer.write_all(&bad).await.unwrap();
            assert_eq!(
                conn.recv(Duration::from_secs(1)).await.unwrap_err().code,
                ErrorCode::Network
            );
            assert!(
                conn.recv(Duration::from_secs(1)).await.is_err(),
                "and stays ended"
            );
        }
        let (mut conn, peer) = pair(64).await;
        drop(peer);
        assert!(conn.recv(Duration::from_secs(1)).await.is_err());
    }

    #[tokio::test(start_paused = true)]
    async fn silence_times_out_and_waiting_can_be_abandoned() {
        let (mut conn, mut peer) = pair(64).await;
        assert!(conn.recv(Duration::from_secs(30)).await.is_err());
        // An abandoned wait loses nothing: the frame is still there.
        peer.write_all(b"cro").await.unwrap();
        assert!(conn.recv(Duration::from_millis(10)).await.is_err());
        peer.write_all(b"c\x01\x00\x00\x00z").await.unwrap();
        assert_eq!(conn.recv(Duration::from_secs(1)).await.unwrap(), b"z");
    }
}
