//! The wire framing: a 4-byte big-endian length, then that many bytes.

use anyhow::anyhow;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

/// Largest frame accepted once the connection is encrypted: a payload chunk
/// with its protobuf and encryption overhead. Senders use chunks of at most
/// 512 KiB; this leaves room for any that use more.
pub const SANE_FRAME_LENGTH: usize = 5 * 1024 * 1024;

/// Largest frame accepted before the connection is encrypted. Those frames
/// (the connection request and response, the UKEY2 messages) are a few
/// hundred bytes, and a peer nobody has authenticated yet gets to make us
/// hold no more than this per connection.
pub const MAX_HANDSHAKE_FRAME_LENGTH: usize = 32 * 1024;

/// Largest frame accepted once the connection is encrypted but before any
/// payload was accepted: one sharing frame (at most
/// [`MAX_CONTROL_PAYLOAD_LENGTH`](super::payload::MAX_CONTROL_PAYLOAD_LENGTH))
/// with its protobuf and encryption overhead. A sender that nobody has said
/// yes to, and a receiver, which never sends file data, get no more.
pub const MAX_SETUP_FRAME_LENGTH: usize = 260 * 1024;

/// Most bytes asked of the socket in one read.
const READ_CHUNK: usize = 64 * 1024;

/// Reassembles frames from a byte stream.
///
/// [`FrameReader::read_frame`] is cancel-safe: the bytes of a frame that has
/// only partly arrived stay in the reader, so the future can be dropped (a
/// branch of `tokio::select!` that lost the race) and a later call picks up
/// where it left off. Reading the length prefix and the body with
/// `read_exact`, as the request handlers used to, loses whatever was read
/// when the future is dropped and desynchronises the stream.
#[derive(Debug, Default)]
pub struct FrameReader {
    buf: Vec<u8>,
}

impl FrameReader {
    /// A reader with nothing buffered.
    pub fn new() -> Self {
        Self::default()
    }

    /// Reads the next whole frame from `stream`, refusing one whose length
    /// prefix says more than `max` bytes -- before reading or allocating any
    /// of it. Memory grows only as the body actually arrives, at most
    /// [`READ_CHUNK`] per read, so a length prefix alone buys a peer nothing.
    pub async fn read_frame<R: AsyncRead + Unpin>(
        &mut self,
        stream: &mut R,
        max: usize,
    ) -> Result<Vec<u8>, anyhow::Error> {
        loop {
            if let Some(frame) = self.take_frame(max)? {
                return Ok(frame);
            }
            // Never read past the frame being assembled: what is buffered is
            // at most one frame.
            let want = self.wanted(max)?.min(READ_CHUNK);
            self.buf.reserve(want);
            let n = (&mut *stream)
                .take(want as u64)
                .read_buf(&mut self.buf)
                .await?;
            if n == 0 {
                return Err(anyhow!("connection closed"));
            }
        }
    }

    /// Bytes still missing from the frame being assembled.
    fn wanted(&self, max: usize) -> Result<usize, anyhow::Error> {
        let end = match self.frame_len(max)? {
            None => 4,
            Some(len) => len
                .checked_add(4)
                .ok_or_else(|| anyhow!("frame too long"))?,
        };
        Ok(end.saturating_sub(self.buf.len()))
    }

    /// The length the prefix announces, once all four bytes are in.
    fn frame_len(&self, max: usize) -> Result<Option<usize>, anyhow::Error> {
        let Some(prefix) = self.buf.get(..4) else {
            return Ok(None);
        };
        let len = usize::try_from(u32::from_be_bytes(prefix.try_into()?))?;
        // Ensure the message length is not unreasonably big to avoid allocation attacks
        if len > max.min(SANE_FRAME_LENGTH) {
            return Err(anyhow!("frame length {len} is over the limit"));
        }
        // An empty frame decodes as an empty message, which no state wants;
        // refusing it here keeps a peer from spinning the parser for free.
        if len == 0 {
            return Err(anyhow!("empty frame"));
        }
        Ok(Some(len))
    }

    fn take_frame(&mut self, max: usize) -> Result<Option<Vec<u8>>, anyhow::Error> {
        let Some(len) = self.frame_len(max)? else {
            return Ok(None);
        };
        let end = len
            .checked_add(4)
            .ok_or_else(|| anyhow!("frame too long"))?;
        if self.buf.len() < end {
            return Ok(None);
        }
        // The buffer holds exactly this frame (reads never go past it), so
        // hand the allocation over rather than copying it, and start the
        // next frame from an empty, small buffer.
        let mut frame = std::mem::take(&mut self.buf);
        frame.drain(..4);
        Ok(Some(frame))
    }
}

/// Writes one frame.
pub async fn write_frame<W: AsyncWrite + Unpin>(
    stream: &mut W,
    data: &[u8],
) -> Result<(), anyhow::Error> {
    let length = u32::try_from(data.len())?;
    let mut prefixed = Vec::with_capacity(4 + data.len());
    prefixed.extend_from_slice(&length.to_be_bytes());
    prefixed.extend_from_slice(data);
    stream.write_all(&prefixed).await?;
    stream.flush().await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn frames_survive_arbitrary_splits_and_cancellation() {
        let mut wire = Vec::new();
        for body in [&b"one"[..], &b"2"[..], &[7u8; 70_000][..], &b"four"[..]] {
            wire.extend_from_slice(&(body.len() as u32).to_be_bytes());
            wire.extend_from_slice(body);
        }
        let (mut tx, mut rx) = tokio::io::duplex(64);
        let writer = tokio::spawn(async move {
            for piece in wire.chunks(3) {
                tx.write_all(piece).await.unwrap();
            }
        });
        let mut reader = FrameReader::new();
        let mut got = Vec::new();
        while got.len() < 4 {
            // Drop the read future often, mid-frame.
            tokio::select! {
                f = reader.read_frame(&mut rx, SANE_FRAME_LENGTH) => got.push(f.unwrap()),
                _ = tokio::task::yield_now() => {}
            }
        }
        writer.await.unwrap();
        assert_eq!(got[0], b"one");
        assert_eq!(got[1], b"2");
        assert_eq!(got[2], vec![7u8; 70_000]);
        assert_eq!(got[3], b"four");
    }

    #[tokio::test]
    async fn oversized_lengths_are_refused_before_reading_the_body() {
        for (len, max) in [
            (u32::MAX, SANE_FRAME_LENGTH),
            (SANE_FRAME_LENGTH as u32 + 1, usize::MAX),
            (
                MAX_HANDSHAKE_FRAME_LENGTH as u32 + 1,
                MAX_HANDSHAKE_FRAME_LENGTH,
            ),
        ] {
            let (mut tx, mut rx) = tokio::io::duplex(64);
            tx.write_all(&len.to_be_bytes()).await.unwrap();
            let err = FrameReader::new()
                .read_frame(&mut rx, max)
                .await
                .unwrap_err();
            assert!(err.to_string().contains("over the limit"), "{len}");
        }
    }

    #[tokio::test]
    async fn empty_frames_are_refused() {
        let (mut tx, mut rx) = tokio::io::duplex(64);
        tx.write_all(&0u32.to_be_bytes()).await.unwrap();
        let err = FrameReader::new()
            .read_frame(&mut rx, SANE_FRAME_LENGTH)
            .await
            .unwrap_err();
        assert!(err.to_string().contains("empty"));
    }

    #[tokio::test]
    async fn a_trickled_length_prefix_allocates_nothing_up_front() {
        let (mut tx, mut rx) = tokio::io::duplex(64);
        tx.write_all(&(SANE_FRAME_LENGTH as u32).to_be_bytes())
            .await
            .unwrap();
        tx.write_all(&[0u8; 10]).await.unwrap();
        let mut reader = FrameReader::new();
        let r = tokio::time::timeout(
            std::time::Duration::from_millis(50),
            reader.read_frame(&mut rx, SANE_FRAME_LENGTH),
        )
        .await;
        assert!(r.is_err(), "still waiting for the body");
        // What arrived plus one read ahead, amortised -- not the 5 MiB announced.
        assert!(reader.buf.capacity() <= 2 * (14 + READ_CHUNK));
    }
}
