//! Reassembly of BYTES payloads: the sharing frames that set a transfer up,
//! and texts.
//!
//! Every size here is the peer's claim and is checked before it is used:
//! a declared size below zero is refused (it used to become a huge
//! `usize` in `Vec::with_capacity` and abort the process), nothing is
//! allocated from a declared size (only from bytes that arrived), a
//! payload never grows past what it declared, all its chunks must declare
//! the same size, and only a few payloads may be in flight at once.

use std::collections::HashMap;

use anyhow::anyhow;

use crate::location_nearby_connections::payload_transfer_frame::PayloadChunk;

/// Largest byte payload other than a text: the sharing frames that set a
/// transfer up (introduction, response, cancel, ...). An introduction with
/// a thousand files and long names stays well under this.
pub const MAX_CONTROL_PAYLOAD_LENGTH: i64 = 256 * 1024;

/// Largest text payload taken.
pub const MAX_TEXT_PAYLOAD_LENGTH: i64 = 64 * 1024;

/// Most byte payloads being reassembled at once. Senders send one at a
/// time (a text may overlap a control frame); each payload id used to get
/// a buffer of its own, without limit.
pub const MAX_PENDING_BYTE_PAYLOADS: usize = 2;

/// Most files one introduction may offer.
pub const MAX_INTRODUCTION_FILES: usize = 1000;

/// A byte payload being reassembled.
#[derive(Debug, Default)]
pub struct BytePayload {
    declared: usize,
    data: Vec<u8>,
}

/// Adds one chunk of byte payload `id`, which declares `total_size` bytes
/// and may be at most `max` bytes. Returns the whole payload once its last
/// chunk is in, and forgets it.
pub fn assemble(
    buffers: &mut HashMap<i64, BytePayload>,
    id: i64,
    total_size: i64,
    max: i64,
    chunk: &PayloadChunk,
) -> Result<Option<Vec<u8>>, anyhow::Error> {
    let result = assemble_inner(buffers, id, total_size, max, chunk);
    if !matches!(result, Ok(None)) {
        buffers.remove(&id);
    }
    result
}

fn assemble_inner(
    buffers: &mut HashMap<i64, BytePayload>,
    id: i64,
    total_size: i64,
    max: i64,
    chunk: &PayloadChunk,
) -> Result<Option<Vec<u8>>, anyhow::Error> {
    if total_size < 0 {
        return Err(anyhow!("byte payload declares a negative size"));
    }
    if total_size > max {
        return Err(anyhow!("byte payload too large: {total_size} bytes"));
    }
    let declared = usize::try_from(total_size)?;
    if !buffers.contains_key(&id) && buffers.len() >= MAX_PENDING_BYTE_PAYLOADS {
        return Err(anyhow!("too many byte payloads in flight"));
    }
    let buffer = buffers.entry(id).or_insert_with(|| BytePayload {
        declared,
        data: Vec::new(),
    });
    if buffer.declared != declared {
        return Err(anyhow!("byte payload changed its declared size"));
    }
    if chunk.offset() != i64::try_from(buffer.data.len())? {
        return Err(anyhow!(
            "Unexpected chunk offset: {}, expected: {}",
            chunk.offset(),
            buffer.data.len()
        ));
    }
    let body = chunk.body();
    let after = buffer
        .data
        .len()
        .checked_add(body.len())
        .ok_or_else(|| anyhow!("byte payload overflow"))?;
    if after > buffer.declared {
        return Err(anyhow!("byte payload longer than it declared"));
    }
    buffer.data.extend_from_slice(body);

    if (chunk.flags() & 1) == 1 {
        if buffer.data.len() != buffer.declared {
            return Err(anyhow!("byte payload shorter than it declared"));
        }
        return Ok(Some(std::mem::take(&mut buffer.data)));
    }
    Ok(None)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn chunk(offset: i64, body: &[u8], last: bool) -> PayloadChunk {
        PayloadChunk {
            offset: Some(offset),
            flags: Some(i32::from(last)),
            body: Some(body.to_vec()),
            ..Default::default()
        }
    }

    #[test]
    fn a_payload_reassembles_and_is_forgotten() {
        let mut b = HashMap::new();
        assert_eq!(
            assemble(&mut b, 1, 5, 10, &chunk(0, b"he", false)).unwrap(),
            None
        );
        assert_eq!(
            assemble(&mut b, 1, 5, 10, &chunk(2, b"llo", true)).unwrap(),
            Some(b"hello".to_vec())
        );
        assert!(b.is_empty());
    }

    #[test]
    fn negative_huge_and_changing_sizes_are_refused() {
        let mut b = HashMap::new();
        assert!(assemble(&mut b, 1, -1, 10, &chunk(0, b"", true)).is_err());
        assert!(assemble(&mut b, 1, i64::MIN, 10, &chunk(0, b"", true)).is_err());
        assert!(assemble(&mut b, 1, 11, 10, &chunk(0, b"", true)).is_err());
        assert!(
            assemble(&mut b, 2, 4, 10, &chunk(0, b"ab", false))
                .unwrap()
                .is_none()
        );
        assert!(assemble(&mut b, 2, 9, 10, &chunk(2, b"cd", true)).is_err());
        assert!(b.is_empty(), "a refused payload is dropped");
    }

    #[test]
    fn a_payload_cannot_outgrow_its_declared_size() {
        let mut b = HashMap::new();
        assert!(assemble(&mut b, 1, 3, 10, &chunk(0, b"abcd", false)).is_err());
        assert!(
            assemble(&mut b, 1, 3, 10, &chunk(0, b"ab", false))
                .unwrap()
                .is_none()
        );
        assert!(assemble(&mut b, 1, 3, 10, &chunk(2, b"cd", false)).is_err());
        assert!(assemble(&mut b, 1, 3, 10, &chunk(0, b"a", true)).is_err());
    }

    #[test]
    fn short_payloads_and_bad_offsets_are_refused() {
        let mut b = HashMap::new();
        assert!(assemble(&mut b, 1, 3, 10, &chunk(0, b"ab", true)).is_err());
        assert!(assemble(&mut b, 1, 3, 10, &chunk(1, b"ab", false)).is_err());
    }

    #[test]
    fn only_a_few_payloads_are_in_flight() {
        let mut b = HashMap::new();
        for id in 0..MAX_PENDING_BYTE_PAYLOADS as i64 {
            assert!(
                assemble(&mut b, id, 5, 10, &chunk(0, b"a", false))
                    .unwrap()
                    .is_none()
            );
        }
        assert!(assemble(&mut b, 99, 5, 10, &chunk(0, b"a", false)).is_err());
        assert_eq!(b.len(), MAX_PENDING_BYTE_PAYLOADS);
    }
}
