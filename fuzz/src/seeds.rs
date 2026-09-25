//! The committed seeds of the two Quick Share targets, as code.
//!
//! Their inputs are decoded with `arbitrary`, so a seed is only as good as
//! the script it decodes to, and a byte string nobody can read is no seed
//! to review. Here each seed is written as the script it is -- a sender
//! following the protocol to a received file, a text, a refusal, a spoiled
//! seal -- and encoded into exactly the bytes `arbitrary` 1.4 decodes back
//! into it ([`Enc`]; `every_seed_decodes_to_its_script` proves it, and fails
//! first if `arbitrary` changes its format). `every_seed_reaches_its_state`
//! proves each still gets where it is meant to.
//!
//! Regenerate `fuzz/seeds/quickshare_{frame,handshake}/` after changing a
//! seed or an input type, from `fuzz/`:
//!
//! ```sh
//! SUKKULA_FUZZ_WRITE_SEEDS=seeds cargo test --lib seeds::write
//! ```

use arbitrary::{Arbitrary, Unstructured};

use crate::handshake::{self, Finish, Init, Key, Pad, Request, Script};
use crate::quickshare::{
    Chunks, Enum, FileMeta, FrameInput, Id, Intro, Sharing, Step, Tamper, TextMeta, run_frames,
};

/// Builds the bytes `arbitrary` decodes a value from. Fixed-size values
/// are read from the front, in field order, little-endian; an enum is a
/// `u32` whose top bits pick the variant; an `Option` or each element of a
/// `Vec` is announced by a `bool` byte; a string's length is taken from
/// the back of the input, one byte while the input is at most 256 bytes --
/// which every seed here is, so lengths are exact.
#[derive(Default)]
struct Enc {
    head: Vec<u8>,
    lengths: Vec<u8>,
}

impl Enc {
    fn byte(&mut self, b: u8) {
        self.head.push(b);
    }

    fn bool(&mut self, b: bool) {
        self.byte(u8::from(b));
    }

    fn le(&mut self, b: &[u8]) {
        self.head.extend_from_slice(b);
    }

    /// Variant `i` of `n`: the middle of its range.
    fn variant(&mut self, i: u32, n: u32) {
        let v = ((u64::from(2 * i + 1) << 32) / u64::from(2 * n)) as u32;
        self.le(&v.to_le_bytes());
    }

    fn str(&mut self, s: &str) {
        self.lengths
            .push(u8::try_from(s.len()).expect("a short string"));
        self.le(s.as_bytes());
    }

    fn bytes(&mut self, b: &[u8]) {
        for x in b {
            self.bool(true);
            self.byte(*x);
        }
        self.bool(false);
    }

    fn opt<T>(&mut self, o: &Option<T>, f: impl FnOnce(&mut Enc, &T)) {
        self.bool(o.is_some());
        if let Some(v) = o {
            f(self, v);
        }
    }

    fn list<T>(&mut self, items: &[T], f: impl Fn(&mut Enc, &T)) {
        for x in items {
            self.bool(true);
            f(self, x);
        }
        self.bool(false);
    }

    fn finish(mut self) -> Vec<u8> {
        let len = self.head.len() + self.lengths.len();
        assert!(len <= 256, "a seed of {len} bytes: keep them to 256");
        self.lengths.reverse();
        self.head.extend(self.lengths);
        self.head
    }
}

fn enum_(e: &mut Enc, v: &Enum) {
    match v {
        Enum::Small(n) => {
            e.variant(0, 2);
            e.byte(*n);
        }
        Enum::Any(n) => {
            e.variant(1, 2);
            e.le(&n.to_le_bytes());
        }
    }
}

fn id(e: &mut Enc, v: &Id) {
    match v {
        Id::Offered(n) => {
            e.variant(0, 3);
            e.byte(*n);
        }
        Id::Text => e.variant(1, 3),
        Id::Any(n) => {
            e.variant(2, 3);
            e.le(&n.to_le_bytes());
        }
    }
}

fn chunks(e: &mut Enc, v: &Chunks) {
    match v {
        Chunks::Whole => e.variant(0, 4),
        Chunks::Split(n) => {
            e.variant(1, 4);
            e.byte(*n);
        }
        Chunks::Lying(n) => {
            e.variant(2, 4);
            e.le(&n.to_le_bytes());
        }
        Chunks::Unfinished => e.variant(3, 4),
    }
}

fn tamper(e: &mut Enc, v: &Tamper) {
    match v {
        Tamper::Sequence(d) => {
            e.variant(0, 5);
            e.le(&d.to_le_bytes());
        }
        Tamper::Signature(b) => {
            e.variant(1, 5);
            e.byte(*b);
        }
        Tamper::Ciphertext(b) => {
            e.variant(2, 5);
            e.le(&b.to_le_bytes());
        }
        Tamper::Schemes(a, b) => {
            e.variant(3, 5);
            enum_(e, a);
            enum_(e, b);
        }
        Tamper::Iv(n) => {
            e.variant(4, 5);
            e.byte(*n);
        }
    }
}

fn file_meta(e: &mut Enc, f: &FileMeta) {
    e.opt(&f.name, |e, s| e.str(s));
    e.opt(&f.payload_id, |e, n| e.le(&n.to_le_bytes()));
    e.opt(&f.size, |e, n| e.le(&n.to_le_bytes()));
    e.opt(&f.mime, |e, s| e.str(s));
    e.opt(&f.kind, enum_);
}

fn text_meta(e: &mut Enc, t: &TextMeta) {
    e.opt(&t.title, |e, s| e.str(s));
    e.opt(&t.payload_id, |e, n| e.le(&n.to_le_bytes()));
    e.opt(&t.size, |e, n| e.le(&n.to_le_bytes()));
    e.opt(&t.kind, enum_);
}

fn sharing(e: &mut Enc, s: &Sharing) {
    match s {
        Sharing::Introduction(i) => {
            e.variant(0, 8);
            e.list(&i.files, file_meta);
            e.list(&i.texts, text_meta);
            e.bool(i.wifi);
            e.opt(&i.required_package, |e, s| e.str(s));
            e.byte(i.pad);
            e.le(&i.extra.to_le_bytes());
        }
        Sharing::PairedKeyEncryption { signed, hash } => {
            e.variant(1, 8);
            e.bytes(signed);
            e.bytes(hash);
        }
        Sharing::PairedKeyResult(v) => {
            e.variant(2, 8);
            enum_(e, v);
        }
        Sharing::Response(v) => {
            e.variant(3, 8);
            enum_(e, v);
        }
        Sharing::Cancel => e.variant(4, 8),
        Sharing::CertificateInfo => e.variant(5, 8),
        Sharing::Bare(v) => {
            e.variant(6, 8);
            enum_(e, v);
        }
        Sharing::Bytes(b) => {
            e.variant(7, 8);
            e.bytes(b);
        }
    }
}

fn step(e: &mut Enc, s: &Step) {
    match s {
        Step::Sharing {
            id: i,
            frame,
            chunks: c,
        } => {
            e.variant(0, 11);
            e.le(&i.to_le_bytes());
            sharing(e, frame);
            chunks(e, c);
        }
        Step::File {
            id: i,
            offset,
            total,
            body,
            last,
        } => {
            e.variant(1, 11);
            id(e, i);
            e.opt(offset, |e, n| e.le(&n.to_le_bytes()));
            e.opt(total, |e, n| e.le(&n.to_le_bytes()));
            e.bytes(body);
            e.bool(*last);
        }
        Step::Text {
            id: i,
            body,
            chunks: c,
        } => {
            e.variant(2, 11);
            id(e, i);
            e.bytes(body);
            chunks(e, c);
        }
        Step::KeepAlive(ack) => {
            e.variant(3, 11);
            e.bool(*ack);
        }
        Step::Disconnection => e.variant(4, 11),
        Step::Offline(b) => {
            e.variant(5, 11);
            e.bytes(b);
        }
        Step::Tampered { offline, how } => {
            e.variant(6, 11);
            e.bytes(offline);
            tamper(e, how);
        }
        Step::Unsealed(b) => {
            e.variant(7, 11);
            e.bytes(b);
        }
        Step::Raw(b) => {
            e.variant(8, 11);
            e.bytes(b);
        }
        Step::Accept => e.variant(9, 11),
        Step::Decline => e.variant(10, 11),
    }
}

fn frame_input(i: &FrameInput) -> Vec<u8> {
    let mut e = Enc::default();
    e.byte(i.start);
    e.str(&i.name);
    e.opt(&i.pin, |e, p| e.le(&p.to_le_bytes()));
    e.list(&i.steps, step);
    e.finish()
}

fn pad(e: &mut Enc, p: &Pad) {
    match p {
        Pad::Java => e.variant(0, 4),
        Pad::Fixed => e.variant(1, 4),
        Pad::Zeros(n) => {
            e.variant(2, 4);
            e.byte(*n);
        }
        Pad::Junk(n) => {
            e.variant(3, 4);
            e.byte(*n);
        }
    }
}

fn handshake_input(i: &handshake::Input) -> Vec<u8> {
    let mut e = Enc::default();
    match i {
        handshake::Input::Stream(b) => {
            e.variant(0, 2);
            e.bytes(b);
        }
        handshake::Input::Scripted(s) => {
            e.variant(1, 2);
            match &s.request {
                Request::Raw(b) => {
                    e.variant(0, 2);
                    e.bytes(b);
                }
                Request::Built {
                    flags,
                    identity,
                    name,
                    length,
                    cut,
                    frame_type,
                } => {
                    e.variant(1, 2);
                    e.byte(*flags);
                    e.le(identity);
                    e.bytes(name);
                    e.opt(length, |e, n| e.byte(*n));
                    e.opt(cut, |e, n| e.byte(*n));
                    e.opt(frame_type, enum_);
                }
            }
            match &s.init {
                Init::Raw(b) => {
                    e.variant(0, 2);
                    e.bytes(b);
                }
                Init::Built {
                    message_type,
                    version,
                    random,
                    others,
                    p256,
                    wrong_commitment,
                    next_protocol,
                } => {
                    e.variant(1, 2);
                    e.opt(message_type, enum_);
                    e.opt(version, enum_);
                    e.opt(random, |e, b| e.bytes(b));
                    e.list(others, enum_);
                    e.bool(*p256);
                    e.bool(*wrong_commitment);
                    e.opt(next_protocol, |e, s| e.str(s));
                }
            }
            match &s.finish {
                Finish::Raw(b) => {
                    e.variant(0, 2);
                    e.bytes(b);
                }
                Finish::Built { message_type, key } => {
                    e.variant(1, 2);
                    e.opt(message_type, enum_);
                    match key {
                        Key::Valid { seed, x, y } => {
                            e.variant(0, 4);
                            e.le(&seed.to_le_bytes());
                            pad(&mut e, x);
                            pad(&mut e, y);
                        }
                        Key::Coordinates { x, y, key_type } => {
                            e.variant(1, 4);
                            e.bytes(x);
                            e.bytes(y);
                            e.opt(key_type, enum_);
                        }
                        Key::Bytes(b) => {
                            e.variant(2, 4);
                            e.bytes(b);
                        }
                        Key::Missing => e.variant(3, 4),
                    }
                }
            }
            e.opt(&s.response, |e, b| e.bytes(b));
            e.list(&s.steps, step);
        }
    }
    e.finish()
}

// ------------------------------------------------------------- the seeds

fn file(id: i64, name: &str, size: i64) -> FileMeta {
    FileMeta {
        name: Some(name.into()),
        payload_id: Some(id),
        size: Some(size),
        mime: Some("image/jpeg".into()),
        kind: None,
    }
}

fn intro(files: Vec<FileMeta>, texts: Vec<TextMeta>) -> Step {
    Step::Sharing {
        id: 1,
        frame: Sharing::Introduction(Intro {
            files,
            texts,
            wifi: false,
            required_package: None,
            pad: 0,
            extra: 0,
        }),
        chunks: Chunks::Whole,
    }
}

fn text(kind: u8, title: &str) -> TextMeta {
    TextMeta {
        title: Some(title.into()),
        payload_id: Some(3),
        size: Some(5),
        kind: Some(Enum::Small(kind)),
    }
}

fn chunk(i: u8, body: &[u8], last: bool) -> Step {
    Step::File {
        id: Id::Offered(i),
        offset: None,
        total: None,
        body: body.to_vec(),
        last,
    }
}

fn setup() -> Vec<Step> {
    vec![
        Step::Sharing {
            id: 2,
            frame: Sharing::PairedKeyEncryption {
                signed: vec![1],
                hash: vec![2],
            },
            chunks: Chunks::Whole,
        },
        Step::Sharing {
            id: 3,
            frame: Sharing::PairedKeyResult(Enum::Small(2)),
            chunks: Chunks::Split(2),
        },
    ]
}

/// Where each `quickshare_frame` seed is meant to get.
fn frame_seeds() -> Vec<(FrameInput, &'static str)> {
    let at_intro = |steps: Vec<Step>| FrameInput {
        start: 2,
        name: "Pixel 8".into(),
        pin: Some(1234),
        steps,
    };
    let mut from_setup = setup();
    from_setup.extend([
        intro(vec![file(11, "a.jpg", 3)], vec![]),
        Step::KeepAlive(false),
        Step::Accept,
        chunk(0, b"ab", false),
        chunk(0, b"c", true),
    ]);
    vec![
        (
            at_intro(vec![
                intro(vec![file(11, "a.jpg", 5)], vec![]),
                Step::Accept,
                chunk(0, b"hello", true),
            ]),
            "all received",
        ),
        (
            at_intro(vec![
                intro(vec![], vec![text(1, "hi")]),
                Step::Accept,
                Step::Text {
                    id: Id::Text,
                    body: b"hello".to_vec(),
                    chunks: Chunks::Split(2),
                },
            ]),
            "text",
        ),
        (
            at_intro(vec![
                intro(vec![], vec![text(2, "https://example.com/")]),
                Step::Accept,
                Step::Text {
                    id: Id::Text,
                    body: "\u{202e}moc.live\n\n\n\nx".as_bytes().to_vec(),
                    chunks: Chunks::Whole,
                },
            ]),
            "text",
        ),
        (
            FrameInput {
                start: 0,
                name: "\u{202e}evil\u{200b}".into(),
                pin: None,
                steps: from_setup,
            },
            "all received",
        ),
        (
            at_intro(vec![
                intro(
                    vec![file(5, "../../.bashrc", 2), file(6, "\u{202e}gpj.exe", 1)],
                    vec![],
                ),
                Step::Accept,
                chunk(0, b"x", false),
                chunk(1, b"y", true),
                chunk(0, b"z", true),
            ]),
            "all received",
        ),
        (
            at_intro(vec![intro(vec![file(5, "a", -1), file(5, "b", 1)], vec![])]),
            "",
        ),
        (
            at_intro(vec![intro(vec![file(5, "a", 1 << 40)], vec![])]),
            "introduction refused",
        ),
        (
            at_intro(vec![
                intro(vec![file(5, "a", 3)], vec![]),
                chunk(0, b"abc", true),
            ]),
            "introduction",
        ),
        (
            at_intro(vec![intro(vec![file(5, "a", 3)], vec![]), Step::Decline]),
            "introduction",
        ),
        (
            at_intro(vec![
                intro(vec![file(5, "a", 3)], vec![]),
                Step::Accept,
                Step::Sharing {
                    id: 9,
                    frame: Sharing::Cancel,
                    chunks: Chunks::Whole,
                },
            ]),
            "accepted",
        ),
        (
            at_intro(vec![Step::Sharing {
                id: 1,
                frame: Sharing::Introduction(Intro {
                    files: vec![file(5, "a", 3)],
                    texts: vec![],
                    wifi: false,
                    required_package: None,
                    pad: 1,
                    extra: 995,
                }),
                chunks: Chunks::Whole,
            }]),
            // rqs_lib takes up to 1000 files; S6 up to 500.
            "introduction refused",
        ),
        (
            at_intro(vec![Step::Sharing {
                id: 1,
                frame: Sharing::Introduction(Intro {
                    files: vec![file(5, "a", 3)],
                    texts: vec![],
                    wifi: false,
                    required_package: None,
                    pad: 1,
                    extra: 499,
                }),
                chunks: Chunks::Split(4),
            }]),
            "introduction",
        ),
        (
            at_intro(vec![Step::Sharing {
                id: 1,
                frame: Sharing::Introduction(Intro {
                    files: vec![file(5, "a", 3)],
                    texts: vec![],
                    wifi: true,
                    required_package: None,
                    pad: 0,
                    extra: 0,
                }),
                chunks: Chunks::Whole,
            }]),
            "",
        ),
        (
            at_intro(vec![
                Step::KeepAlive(true),
                Step::Tampered {
                    offline: vec![8, 1],
                    how: Tamper::Sequence(1),
                },
            ]),
            "",
        ),
        (
            at_intro(vec![Step::Raw(vec![
                0, 0, 0, 3, 1, 2, 3, 0x7f, 0xff, 0xff, 0xff,
            ])]),
            "",
        ),
    ]
}

/// Where each `quickshare_handshake` seed is meant to get.
fn handshake_seeds() -> Vec<(handshake::Input, &'static str)> {
    let script = |key: Key, steps: Vec<Step>| {
        handshake::Input::Scripted(Box::new(Script {
            request: Request::Built {
                flags: 0b0010,
                identity: [7; 16],
                name: b"Pixel".to_vec(),
                length: None,
                cut: None,
                frame_type: None,
            },
            init: Init::Built {
                message_type: None,
                version: None,
                random: None,
                others: vec![],
                p256: true,
                wrong_commitment: false,
                next_protocol: None,
            },
            finish: Finish::Built {
                message_type: None,
                key,
            },
            response: None,
            steps,
        }))
    };
    let valid = |x, y| Key::Valid { seed: 7, x, y };
    let mut to_file = setup();
    to_file.extend([
        intro(vec![file(4, "a", 1)], vec![]),
        Step::Accept,
        chunk(0, b"a", true),
    ]);
    vec![
        (script(valid(Pad::Java, Pad::Java), to_file), "all received"),
        (
            script(valid(Pad::Fixed, Pad::Zeros(3)), vec![]),
            "channel open",
        ),
        (
            script(valid(Pad::Junk(0x40), Pad::Java), vec![]),
            "client init",
        ),
        (
            script(
                Key::Coordinates {
                    x: vec![1; 3],
                    y: vec![0; 2],
                    key_type: None,
                },
                vec![],
            ),
            "client init",
        ),
        (script(Key::Missing, vec![]), "client init"),
        (
            script(Key::Bytes(vec![0x08, 0x01, 0x12]), vec![]),
            "client init",
        ),
        (
            handshake::Input::Scripted(Box::new(Script {
                request: Request::Built {
                    flags: 0xff,
                    identity: [0; 16],
                    name: vec![0xc3, 0x28, b'x'],
                    length: Some(200),
                    cut: None,
                    frame_type: None,
                },
                init: Init::Raw(vec![]),
                finish: Finish::Raw(vec![]),
                response: None,
                steps: vec![],
            })),
            "",
        ),
        (
            handshake::Input::Scripted(Box::new(Script {
                request: Request::Built {
                    flags: 0b0110,
                    identity: [1; 16],
                    name: "\u{202e}Evil".as_bytes().to_vec(),
                    length: None,
                    cut: None,
                    frame_type: None,
                },
                init: Init::Built {
                    message_type: None,
                    version: Some(Enum::Small(2)),
                    random: Some(vec![1; 4]),
                    others: vec![Enum::Small(1)],
                    p256: true,
                    wrong_commitment: true,
                    next_protocol: Some("AES_256_CBC-HMAC_SHA256".into()),
                },
                finish: Finish::Built {
                    message_type: None,
                    key: Key::Missing,
                },
                response: None,
                steps: vec![],
            })),
            "connection request",
        ),
        (
            handshake::Input::Stream(vec![0, 0, 0, 4, 8, 1, 0x12, 0, 0x7f, 0xff, 0xff, 0xff]),
            "",
        ),
    ]
}

fn decodes_to<'a, T: Arbitrary<'a> + PartialEq + std::fmt::Debug>(bytes: &'a [u8], want: &T) {
    let got = T::arbitrary(&mut Unstructured::new(bytes)).expect("decodes");
    assert_eq!(&got, want);
}

#[test]
fn every_seed_decodes_to_its_script() {
    for (input, _) in frame_seeds() {
        decodes_to(&frame_input(&input), &input);
    }
    for (input, _) in handshake_seeds() {
        decodes_to(&handshake_input(&input), &input);
    }
}

#[test]
fn every_seed_reaches_its_state() {
    for (input, want) in frame_seeds() {
        let reached = run_frames(&input);
        assert!(
            want.is_empty() || reached.contains(&want),
            "{want:?} not in {reached:?}: {input:?}"
        );
    }
    for (input, want) in handshake_seeds() {
        let reached = handshake::run(&input);
        assert!(
            want.is_empty() || reached.contains(&want),
            "{want:?} not in {reached:?}: {input:?}"
        );
    }
}

/// Writes the seeds, when asked to: see the module docs.
#[test]
// The S3 ban on writing files is for shipped code; this is a test of the
// harness, run by hand, writing only under the directory it is given.
#[allow(clippy::disallowed_methods)]
fn write() {
    let Some(dir) = std::env::var_os("SUKKULA_FUZZ_WRITE_SEEDS") else {
        return;
    };
    let dir = std::path::PathBuf::from(dir);
    let sets: [(&str, Vec<Vec<u8>>); 2] = [
        (
            "quickshare_frame",
            frame_seeds().iter().map(|(i, _)| frame_input(i)).collect(),
        ),
        (
            "quickshare_handshake",
            handshake_seeds()
                .iter()
                .map(|(i, _)| handshake_input(i))
                .collect(),
        ),
    ];
    for (target, seeds) in sets {
        let d = dir.join(target);
        std::fs::create_dir_all(&d).expect("seed directory");
        for (n, bytes) in seeds.iter().enumerate() {
            std::fs::write(d.join(format!("{n:02}")), bytes).expect("seed written");
        }
    }
}
