//! A croc sender's file list, the offer the user is asked about, and a
//! receiver's request for a file of ours
//! (`sukkula_engine::croc::fuzzing::{file_list, file_request}`).
//!
//! Both come sealed from whoever has the code. Asserted, against the JSON
//! read independently through `serde_json::Value`:
//!
//! - a list is taken only with no hash or XXH64, and then its files are
//!   exactly the sender's, in its order and by their index there, each
//!   with the name and size it declared -- symbolic links left out, as
//!   they are never asked for -- and at least one; a text is one file of
//!   at most 64 KiB, offered as `text.txt`; chunks are deflated unless the
//!   sender said not;
//! - the offer is croc's, from "croc", with those names and sizes, and
//!   what validates keeps every S-rule (`sukkula_fuzz::assert_offer`);
//! - a request against our files is for one that has bytes, and asks for
//!   its chunks in order, none twice, none past its end: all of them, or
//!   the aligned ranges it named.
#![no_main]
// `fuzz_target!` itself writes the input to RUST_LIBFUZZER_DEBUG_PATH when
// that is set; the S3 ban is for shipped code, and this is the harness.
#![allow(clippy::disallowed_methods)]

use libfuzzer_sys::fuzz_target;
use serde_json::Value;
use sukkula_core::Protocol;
use sukkula_core::limits::MAX_MESSAGE_BYTES;
use sukkula_core::offer::Offer;
use sukkula_engine::croc::PEER_LABEL;
use sukkula_engine::croc::fuzzing::{self, CHUNK_BYTES, FileList};
use sukkula_fuzz::{as_struct, assert_offer, value_of};

/// The sizes of the files a request is read against: empty, one byte, a
/// chunk, a chunk and a byte, three chunks and a bit.
const SIZES: [u64; 5] = [0, 1, 32_768, 32_769, 3 * 32_768 + 5];

const SENDER: [&str; 9] = [
    "FilesToTransfer",
    "TotalNumberFolders",
    "MachineID",
    "Ask",
    "SendingText",
    "NoCompress",
    "HashAlgorithm",
    "ReconnectVersion",
    "NextReconnectRoom",
];
const FILE: [&str; 7] = ["n", "fr", "h", "s", "m", "sy", "md"];
const REQUEST: [&str; 4] = [
    "CurrentFileChunkRanges",
    "FilesToTransferCurrentNum",
    "MachineID",
    "ReconnectVersion",
];

fn text_of<'a>(v: &'a Value, key: &str) -> &'a str {
    v.get(key).and_then(Value::as_str).unwrap_or_default()
}

fn check_list(data: &[u8], list: &FileList) {
    let Some(v) = value_of(data, "a list") else {
        return;
    };
    let v = as_struct(v, &SENDER);
    let hash = text_of(&v, "HashAlgorithm");
    assert!(hash.is_empty() || hash == "xxhash", "hash {hash:?} taken");
    let wanted: Vec<(usize, String, u64)> = v
        .get("FilesToTransfer")
        .and_then(Value::as_array)
        .expect("a list without files")
        .iter()
        .map(|f| as_struct(f.clone(), &FILE))
        .enumerate()
        .filter(|(_, f)| text_of(f, "sy").is_empty())
        .map(|(i, f)| {
            let size = f.get("s").map_or(Some(0), Value::as_u64).expect("a size");
            (i, text_of(&f, "n").to_owned(), size)
        })
        .collect();
    assert!(!wanted.is_empty(), "an empty list taken");
    assert_eq!(list.files, wanted, "the files are not the sender's");
    let flag = |k| v.get(k).and_then(Value::as_bool).unwrap_or(false);
    assert_eq!(list.text, flag("SendingText"));
    assert_eq!(list.compressed, !flag("NoCompress"));
    if list.text {
        assert_eq!(list.files.len(), 1, "a text of several files");
        let max = u64::try_from(MAX_MESSAGE_BYTES).unwrap();
        assert!(list.files.iter().all(|f| f.2 <= max), "a long text");
    }

    let offer = &list.offer;
    assert_eq!(offer.protocol, Protocol::Croc);
    assert_eq!(offer.sender, PEER_LABEL);
    assert!(offer.text.is_none() && offer.pin.is_none() && offer.model.is_none());
    let shown: Vec<(&str, i128)> = offer
        .files
        .iter()
        .map(|f| (f.name.as_str(), f.size))
        .collect();
    let sent: Vec<(&str, i128)> = list
        .files
        .iter()
        .map(|(_, n, s)| {
            (
                if list.text { "text.txt" } else { n.as_str() },
                i128::from(*s),
            )
        })
        .collect();
    assert_eq!(shown, sent, "the offer is not the list");
    if let Ok(o) = Offer::validate(offer.clone()) {
        assert_offer(&o);
    }
}

fn check_request(data: &[u8], index: usize, chunks: &[u64]) {
    let Some(v) = value_of(data, "a request") else {
        return;
    };
    let v = as_struct(v, &REQUEST);
    let named = v
        .get("FilesToTransferCurrentNum")
        .map_or(Some(0), Value::as_u64)
        .expect("an index");
    assert_eq!(
        u64::try_from(index).unwrap(),
        named,
        "another file than asked"
    );
    let size = SIZES[index];
    assert!(size > 0, "an empty file served");
    let chunk = u64::try_from(CHUNK_BYTES).unwrap();
    let count = size.div_ceil(chunk);
    assert!(
        chunks.windows(2).all(|w| w[0] < w[1]),
        "chunks out of order or twice"
    );
    assert!(chunks.iter().all(|c| *c < count), "a chunk past the end");
    let ranges: Vec<u64> = v
        .get("CurrentFileChunkRanges")
        .and_then(Value::as_array)
        .map(|r| r.iter().map(|n| n.as_u64().expect("a range")).collect())
        .unwrap_or_default();
    match ranges.split_first() {
        None => assert_eq!(chunks, (0..count).collect::<Vec<_>>(), "not the whole file"),
        Some((first, pairs)) => {
            assert_eq!(*first, chunk, "ranges in another unit");
            let mut asked = Vec::new();
            for pair in pairs.chunks(2) {
                let [start, n] = pair else {
                    panic!("a range without its count taken")
                };
                assert_eq!(start % chunk, 0, "an unaligned range");
                assert!(*n > 0, "an empty range");
                asked.extend(start / chunk..start / chunk + n);
            }
            assert_eq!(chunks, asked, "not the chunks asked for");
        }
    }
}

fuzz_target!(|data: &[u8]| {
    if let Ok(list) = fuzzing::file_list(data) {
        check_list(data, &list);
    }
    if let Some((index, chunks)) = fuzzing::file_request(data, &SIZES) {
        check_request(data, index, &chunks);
    }
});
