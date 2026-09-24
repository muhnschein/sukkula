# open-quickshare: defects found, and the fixes Sukkula carries

**Status: draft. Nothing here has been reported or filed anywhere.** It is
the text of the upstream reports as they would be sent, kept next to the
patches so that each can be dropped from `third_party/rqs_lib.patches/` once
upstream has an equivalent fix.

- Upstream: <https://github.com/ignotusbucius/open-quickshare>, `core_lib/`,
  commit `5a31145163ee22ab9cf1c7d3dffd74355febe93f`
  (`third_party/rqs_lib.UPSTREAM`). Every `file:line` below is at that
  commit, relative to `core_lib/`.
- Sukkula's copy: `third_party/rqs_lib/` = upstream plus the 18 patches in
  `third_party/rqs_lib.patches/`, applied in order;
  `third_party/rqs_lib.patches/check.sh` (and `ci/vendor-check.sh` in CI)
  prove it.
- Q1–Q7 are the findings of Sukkula's spec (§5). R1–R17 were found while
  patching.

**Suggested handling.** Q3, Q4, R1, R2, R3, R4, R7 and R12 can be
triggered by anyone on the same network as a receiver that is visible,
with no user interaction; Q1 needs one tap on "accept" for a name the
prompt shows, and Q6 a peer the user is exchanging files with. These
should go to the maintainer privately first (a GitHub security advisory
on the repository), with the rest as ordinary issues once fixes are out.

Several of Sukkula's patches do more than an upstream fix would need,
because Sukkula embeds the library in a different shape (0015 turns it
into a library the application drives, 0004/0005 remove the BLE
transport). For each finding the report names the smallest fix upstream
could take in its own design, and the Sukkula patch that carries our fix.

## The patch series

| Patch | Concern | Findings |
| --- | --- | --- |
| 0001-drop-unshipped-files | TypeScript bindings, examples, demo binary, upstream lockfile, nightly rustfmt config | -- |
| 0002-committed-protobuf | generated protobuf committed, no `protoc` at build time | -- |
| 0003-q7-system-libdbus | no `dbus/vendored` | Q7 |
| 0004-remove-ble-transport | BLE transport and bandwidth upgrade removed (Sukkula is LAN only) | Q6 (callers) |
| 0005-q6-no-network-changes | `hotspot.rs`/`nmcli` gone, tokio `process` off, no role-switch bait | Q6, R14 |
| 0006-rustcrypto-aes-cbc | `aes` + `cbc` instead of libaes | R2 |
| 0007-crates-io-dependencies | `mdns-sd` 0.21 and `if-addrs` from crates.io | R17 |
| 0008-q3-q4-q5-size-checks | every declared size checked, BYTES payloads capped | Q3, Q4, Q5, R8 |
| 0009-s5-consent-first | no payload before the user accepted | R3, R4 |
| 0010-f-qs5-refuse-wifi-credentials | Wi-Fi credentials refused | R5 |
| 0011-s4-s6-frame-limits-and-timeouts | frame limits by phase, cancel-safe reads, read timeouts | R7 |
| 0012-no-panics-on-peer-input | no `unwrap` on the handshake path, correct P-256 decoding | R1, R6, R16 |
| 0013-macs-schemes-and-alerts | constant-time MAC, schemes checked, UKEY2 alerts | R15 |
| 0014-s9-logs-without-user-data | no names, paths, texts, PINs in logs | R13 |
| 0015-q1-q2-embedding-api | library writes no files; the application drives it | Q1, Q2, R9, R10, R11 |
| 0016-s7-mdns-to-the-lan | mDNS on accepted interfaces, bounded, no probing, stoppable | R12 |
| 0017-crate-metadata-and-licence | licence file and `license` field | -- |
| 0018-tests | hostile-frame unit tests, loopback test | -- |

---

## Q1. A received file's name can leave the download directory

**Where.** `src/hdl/inbound.rs:1327-1331` (`process_introduction`):
`dest_file_name(file.name(), file.mime_type())` (`:77`) keeps the peer's
`FileMetadata.name` as it is, apart from its extension, and
`dest.push(&resolved_name)` joins it onto `get_download_dir()`.

**Impact.** The name is chosen by the sender. `../` components write outside
the download directory, anywhere the user can write (`../.bashrc`,
`../.config/autostart/x.desktop`); an absolute name is worse, because
`PathBuf::push` of an absolute path replaces the base altogether. The
consent prompt shows the name, but a user accepting "../.bashrc" from a
phone is not the defence.

**Upstream fix.** Reduce the name to one path component before anything
else: take the last segment after `/` and `\`, drop NUL and control
characters, refuse or replace `.`, `..`, empty names and leading dots, and
cap the length; build the path from the download directory and that
component only.

**Sukkula.** Patch 0015: the library no longer builds paths or writes files
at all. It hands the application `Introduction` (names untouched) and
`FileChunk`s; Sukkula's inbox reduces names to one safe component (spec S1)
and writes (S3). A unit test (`inbound_tests.rs`,
`names_reach_the_application_as_sent_and_nothing_else`) and an engine test
(`hostile_names_become_one_safe_component`) cover `../../x`, absolute
paths, `\`, `..`, leading dots, bidi overrides and NUL.

## Q2. `exists()` now, `File::create` later

**Where.** `src/hdl/inbound.rs:1334-1372` picks a free "name (n).ext" by
probing `dest.exists()` when the introduction arrives;
`src/hdl/inbound.rs:1533` (`accept_transfer`) creates it with
`File::create` when the user answers.

**Impact.** The gap is as long as the user takes to answer. Anything created
at that path meanwhile (another transfer with the same name, or a symlink)
is truncated or followed: `File::create` is `O_CREAT|O_TRUNC` without
`O_EXCL` or `O_NOFOLLOW`. Two concurrent offers with the same name pick
the same destination.

**Upstream fix.** Create the file when it is picked, with
`OpenOptions::new().write(true).create_new(true)` (and `O_NOFOLLOW`),
retrying the next "(n)" on `AlreadyExists`; better, write to a temporary
name in the directory and link it into place when complete.

**Sukkula.** Patch 0015 (no file code left in the library); the inbox stages
with `O_CREAT|O_EXCL`, mode 0600, and links into place without overwriting
(S3).

## Q3. A negative BYTES payload size panics the connection

**Where.** `src/hdl/inbound.rs:904` refuses `total_size() > 5 MiB` only;
`:915` then does `Vec::with_capacity(header.total_size() as usize)`. The
sender side does the same at `src/hdl/outbound.rs:655-668`.

**Impact.** A negative `i64` cast to `usize` is above `isize::MAX`, so
`Vec::with_capacity` panics with "capacity overflow": the connection task
dies (the whole process, in a build with `panic = "abort"`). Any peer can
send it right after the handshake, before the user sees anything; a
receiver can do it to a sender.

**Upstream fix.** Refuse `total_size < 0` along with the upper bound, and do
not preallocate from a declared size at all.

**Sukkula.** Patch 0008 (`hdl/payload.rs`, `assemble`). Tests:
`payload.rs` `negative_huge_and_changing_sizes_are_refused`,
`inbound_tests.rs` `byte_payloads_are_bounded`, engine
`payloads_stop_at_their_declared_size`.

## Q4. BYTES payload buffers grow without limit

**Where.** `src/hdl/inbound.rs:913-931` and `src/hdl/outbound.rs:666-684`:
chunks are appended (`buffer.extend(body)`) with no check against the
declared size; a finished buffer is never removed from
`payload_buffers`; any number of payload ids can be open.

**Impact.** Unbounded memory before consent: a peer streams chunks with the
last flag never set, or opens many payload ids.

**Upstream fix.** Refuse a chunk that would take the buffer past its
declared size (and a declared size over a small cap), remove a buffer when
it completes or fails, and bound the number of open payloads.

**Sukkula.** Patch 0008: at most 256 KiB for a sharing frame, 64 KiB for a
text, two payloads in flight, a payload never past its declared size.

## Q5. Negative file sizes wrap the total the user accepts

**Where.** `src/hdl/inbound.rs:1375-1381`: `total_size: file.size()` (an
`i64`), then `total_bytes += info.total_size as u64`.

**Impact.** A file of size -1 adds 2^64-1, which wraps the total; the
consent prompt shows a wrong size. Nothing limits the number of files or
keeps `payload_id`s unique either, so two entries can share one
book-keeping slot and one output file.

**Upstream fix.** Refuse the introduction if any size is negative, cap the
number of files, refuse duplicate payload ids, and sum with checked
arithmetic.

**Sukkula.** Patch 0008 (at most 1000 files, sizes >= 0, unique ids;
Sukkula's own offer check adds 500 files, 8 GiB per file, 16 GiB in all).
Engine test `bad_sizes_are_refused_before_the_user_is_asked`.

## Q6. A peer can make the machine change network

**Where.** `src/hdl/hotspot.rs:82` (`join_wifi`, via `nmcli` at `:106`,
`:162`) joins the Wi-Fi Direct group or hotspot whose SSID and password
the peer sends; `:175` (`start_hotspot`) takes the Wi-Fi interface off its
network to host one. Callers: `src/hdl/outbound.rs:1198` and `:1232`
(send side), `src/hdl/inbound.rs:2098-2105` (receive side). The sender
invites it: `src/hdl/outbound.rs:337-358` claims it can host a Wi-Fi Direct
group and a hotspot and sends the local IP, and `:505-525` presents the
machine as Windows so that the phone asks for the role switch.

**Impact.** A peer decides which network the machine joins, drops it off its
own network, and runs external programs with peer-supplied arguments
(passed as argv, so not a shell injection, but still a peer-driven network
change). The environment knob `PACKET_SEND_META` (`:334`, `:513`) changes
protocol behaviour from outside.

**Upstream fix.** Never join or host a network without an explicit,
per-transfer confirmation from the user naming the network; do not claim
roles the user has not enabled.

**Sukkula.** Patches 0004 (the bandwidth upgrade, the only caller, goes with
the BLE transport) and 0005 (`hotspot.rs` deleted, tokio `process` off,
only `WIFI_LAN` announced, OS type Linux). Spec S8: no process spawning, no
Wi-Fi changes.

## Q7. libdbus is built from source and linked statically on aarch64

**Where.** `Cargo.toml:15-16`:
`[target.'cfg(all(target_arch = "aarch64", target_os = "linux"))'.dependencies] dbus = { version = "0.9", features = ["vendored"] }`.

**Impact.** Through feature unification every D-Bus user in the build
(bluer, btleplug) gets a private static libdbus on the architecture phones
use: a second D-Bus implementation in the process, and one the system's
security updates never reach.

**Upstream fix.** Drop the line; link the system libdbus through pkg-config
(the `dbus` crate's default).

**Sukkula.** Patch 0003. Sukkula's `deny.toml` refuses the `vendored`
feature of `dbus` and `libdbus-sys` from any crate.

---

## R1. An off-curve public key panics the handshake

**Where.** `src/hdl/inbound.rs:1596-1610` and
`src/hdl/outbound.rs:1956-1970` (`finalize_key_exchange`):
`PublicKey::from_encoded_point(&encoded_point).unwrap()`.

**Impact.** `from_encoded_point` is `None` for any point not on P-256, and
the coordinates come straight from the peer's ClientFinish (receiver) or
ServerInit (sender), before anything is authenticated: one frame panics the
connection task. The decoding is also wrong for honest keys: a coordinate
longer than 32 bytes is cut to its last 32 whatever the leading bytes are,
and a shorter one (Java's `BigInteger.toByteArray()` drops leading zeros,
about one key in 128) is used as is, so that handshake fails.

**Upstream fix.** Decode each coordinate as a big-endian integer (strip
leading zeros, refuse more than 32 significant bytes, left-pad to 32) and
return an error for a point not on the curve.

**Sukkula.** Patch 0012 (`utils::decode_p256_point`); test
`p256_coordinates_in_every_encoding`, engine
`a_garbled_handshake_is_dropped`.

## R2. libaes panics on a short IV and accepts any padding

**Where.** `src/hdl/inbound.rs:387-389`, `:825-827`, `:1755-1757` and
`src/hdl/outbound.rs:610-612`, `:1035-1037`, `:2121-2123` use libaes 0.7's
`Cipher::cbc_decrypt`/`cbc_encrypt`. `cbc_decrypt` XORs with `iv[i]` for
`i` in `0..16` without checking the IV's length (libaes `src/lib.rs`,
`xor_with_iv`), and its unpadding takes the last byte as a length without
validating the padding.

**Impact.** The IV comes from the peer's SecureMessage header. The HMAC is
checked first, but the handshake is unauthenticated, so any peer that
completes it has valid keys: a 1-byte IV panics the connection task. Bad
padding is silently accepted. libaes is also a table-based AES.

**Upstream fix.** Use the `aes` and `cbc` crates (checked lengths, PKCS#7
checked, constant time).

**Sukkula.** Patch 0006 (`utils::aes_cbc_encrypt`/`aes_cbc_decrypt`); test
`aes_cbc_round_trips_and_checks_its_inputs`.

## R3. A file chunk before consent panics the receiver

**Where.** `src/hdl/inbound.rs:1078-1083`: a FILE chunk is written with
`file_internal.file.as_ref().unwrap()`, and the file is only created in
`accept_transfer` (`:1527-1536`). Nothing checks that the transfer was
accepted.

**Impact.** A sender that sends its first chunk right after the introduction
panics the receiver's task. With the check in place it would also be the
only thing keeping bytes from being written before consent.

**Upstream fix.** Refuse FILE chunks unless the state is `ReceivingFiles`.

**Sukkula.** Patch 0009; tests `no_payload_byte_before_acceptance`,
`file_bytes_before_consent_are_refused` (loopback), engine
`nothing_is_taken_before_consent`.

## R4. A text is taken and reported before consent

**Where.** `src/hdl/inbound.rs:936-1042`: the BYTES payload of an introduced
text is decoded and stored in the transfer metadata, and the transfer
reported `Finished`, whenever it arrives. `:418` acts on `ConsentAccept`
in any state.

**Impact.** The text reaches the application while the consent prompt is
still up; the answer no longer matters.

**Upstream fix.** Refuse the text payload unless the transfer was accepted,
and accept only while waiting for consent.

**Sukkula.** Patch 0009.

## R5. Wi-Fi passwords are parsed, and logged on error

**Where.** `src/hdl/inbound.rs:1467-1495` offers shared Wi-Fi credentials
like a text; `:981-1012` parses the password out of the payload, and on any
parse error logs it with `error!("{err:#}")`, whose message contains the
whole buffer, password included.

**Impact.** The password lands in the log at error level. Joining the
network is the only use of it (see Q6).

**Upstream fix.** Never put the buffer in the error message. (Sukkula
refuses the payload type outright, spec F-QS5.)

**Sukkula.** Patch 0010 (`UNSUPPORTED_ATTACHMENT_TYPE` before consent);
tests `wifi_credentials_are_refused_before_consent`, engine
`wifi_credentials_are_refused`.

## R6. Keys are unwrapped before there are any

**Where.** `src/hdl/inbound.rs:195` and `src/hdl/outbound.rs:187` start
with `encryption_done: true`; `disconnection` and `send_keepalive` then go
to `encrypt_and_send`, which does `self.state.encrypt_key.as_ref().unwrap()`
(`inbound.rs:1751`). Also `inbound.rs:725` (commitment), `:1611-1618`
(private key, handshake messages), the own key's coordinates
(`inbound.rs:671-672`, `outbound.rs:398-399`), and the sequence counters
(`inbound.rs:1828-1850`, `+= 1` on an `i32`).

**Impact.** Cancelling a transfer that is still handshaking panics; the
rest are reachable only with a broken state machine, but every one is a
panic path on network input.

**Upstream fix.** Start with `encryption_done: false`; turn the `unwrap`s
into errors; use checked arithmetic for the counters.

**Sukkula.** Patch 0012.

## R7. Frames: pre-authentication allocation, lost bytes, no timeouts

**Where.** `src/hdl/inbound.rs:403-473` (`handle`, `_handle`) and
`src/hdl/outbound.rs:221-275`:

- any length up to 5 MiB is accepted in every state and allocated at once
  (`vec![0u8; msg_length]`, `inbound.rs:472`);
- the 4-byte length is read with `read_exact` inside a `tokio::select!`
  that also waits for the channel (`inbound.rs:453`): when the channel
  wins mid-read, the bytes already read are dropped with the future and
  the stream is parsed from the middle of a frame;
- there is no read timeout anywhere; an empty frame is accepted.

**Impact.** Four bytes from an unauthenticated peer buy a 5 MiB allocation
per connection; a peer that connects and sends nothing, or stops mid-frame
or mid-file, holds its task, socket and half-written file forever
(slow-loris); a busy channel corrupts the stream.

**Upstream fix.** A frame reader that keeps partial frames across
cancellation, a small limit until the handshake is done (a few KiB) and
until the transfer is accepted, grows its buffer only as bytes arrive, and
a read timeout (30 s, Nearby's own keep-alive timeout).

**Sukkula.** Patch 0011 (`hdl/frame.rs`); tests in `frame.rs`, engine
`oversized_frame_lengths_are_refused_up_front`,
`stalled_senders_are_given_up_on`.

## R8. A file can end short and still be "finished"

**Where.** `src/hdl/inbound.rs:1069-1108`: an empty chunk with the last flag
removes the file from the transfer and reports `Finished` whatever
`bytes_transferred` is; a non-empty chunk with the last flag never ends
the file; `:1070` adds with unchecked arithmetic.

**Impact.** A truncated file is reported complete; a sender that puts data
in its last chunk leaves the transfer hanging.

**Upstream fix.** End the file on the last chunk, with or without data, and
only at exactly its declared size.

**Sukkula.** Patch 0008; test `file_chunks_stop_at_the_declared_size`.

## R9. The TCP server: no limits, and one error stops it

**Where.** `src/manager.rs:54-141` (`TcpServer::run`): every accepted
connection gets a task with no cap on their number, no filter on the
source address and no bound on its life (see R7); `:132-134` breaks out of
the accept loop, for good, on the first `accept` error (one `EMFILE`);
`:65-80` runs a whole send inside the same loop, so nothing is accepted
while something is sent.

**Impact.** Descriptor and memory exhaustion from one host; receiving stops
until restart after a transient error; a long send blocks receiving.

**Upstream fix.** Cap connections in total and per address, back off and
continue on accept errors, spawn sends.

**Sukkula.** Patch 0015 removes the server (Sukkula's adapter accepts, with
4 connections, 2 per address, its offer rate limit and reach policy).

## R10. The sender: blocking reads, TOCTOU, and a loop that never ends

**Where.** `src/hdl/outbound.rs:831-848` checks `path.is_file()`, then opens
the path; `:1728-1729` reads with blocking `std::fs` I/O on the async
runtime into a 512 KiB buffer regardless of what is left to send;
`:1809` ends a file only when the bytes sent reach the size exactly.

**Impact.** A FIFO swapped in after the check blocks a runtime thread
forever. A file that grows is sent past its announced size; one that
shrinks makes `read` return 0 forever and the sender loops sending empty
chunks. While sending it never reads the socket (`:1670` only polls the
channel), so the receiver's keep-alives go unanswered and its cancel is
not seen.

**Upstream fix.** Open once, `O_NONBLOCK`, and check the handle with
`fstat` (regular file, the announced size); read at most what is left; end
on a short read with an error; read the socket between chunks.

**Sukkula.** Patch 0015 (the application hands over bytes;
`send_file_chunk` refuses to pass the announced size, `finish_file` to end
short); Sukkula's adapter opens `O_NONBLOCK` and checks the handle (engine
test `a_file_swapped_after_the_check_is_refused`).

## R11. Blocking file writes on the async runtime

**Where.** `src/hdl/inbound.rs:1079-1083` (`write_all_at` on a `std::fs::File`).

**Impact.** A slow disk stalls a runtime thread and every task on it.

**Sukkula.** Patch 0015 (the application writes).

## R12. mDNS: all interfaces, connection probing, busy loop, blocking stop

**Where.**

- `src/hdl/mdns.rs:54`, `src/hdl/mdns_discovery.rs:35`: the daemons run on
  every interface, a public mobile-data or VPN one included.
- `src/hdl/mdns_discovery.rs:91`: every announced address is probed with
  `TcpStream::connect` and no timeout, one at a time.
- `:104`: resolved services are cached without limit.
- `:126`: on a channel error it logs and loops; the channel never yields
  again, so it spins a core at 100 %.
- `src/hdl/mdns.rs:89`, `:116`, `:124`: blocking `receiver.recv()` in async
  code; the daemon thread is never shut down.
- `src/utils.rs:102`, `:56-61`: the announced name's length is `as u8`
  (wraps over 255 bytes) and the connection request cuts at 255 bytes
  mid-character.

**Impact.** Any host on the link can make the machine open connections to
addresses of its choosing and stall discovery; memory grows with
announcements; a dead daemon burns CPU; stopping stalls a runtime thread.

**Upstream fix.** Restrict interfaces, report only filtered addresses and do
not probe, bound the cache, end on a closed channel, await the goodbye and
the daemon shutdown asynchronously with a timeout, cut names on a
character boundary.

**Sukkula.** Patch 0016 (`AddrFilter` = Sukkula's reach policy S7: private,
link-local, ULA).

## R13. File names, paths, texts and the PIN in the logs

**Where.** e.g. `src/hdl/inbound.rs:482` (RemoteDeviceInfo), `:878`
(every chunk's file name and folder), `:1186` (whole unhandled frames),
`:1328`, `:1333` (names and destinations), `:1398`, `:1424`, `:1449`
(consent metadata), `:1534` (created file), `:1655` (PIN);
`src/hdl/outbound.rs:832-846`, `:891`, `:1710` (paths); `src/manager.rs:66`
(SendInfo with paths); `src/hdl/mdns.rs:140`,
`src/hdl/mdns_discovery.rs:103`. Peer strings end up in the log verbatim,
e.g. `inbound.rs:663` (the peer's next-protocol string).

**Impact.** Private data in logs, at info level; peers can write into them.

**Sukkula.** Patch 0014 (spec S9).

## R14. The sender claims Wi-Fi roles and an OS it does not have

See Q6 (`outbound.rs:337-358`, `:505-525`). **Sukkula.** Patch 0005.

## R15. HMAC compared in variable time; schemes unchecked; alerts mistyped

**Where.** `src/hdl/inbound.rs:376-381`, `:810-815`,
`src/hdl/outbound.rs:595-600`, `:1024-1029` compare tags with slice
equality; the SecureMessage header's schemes are never checked;
`inbound.rs:1667`, `outbound.rs:2031` put the alert type in
`Ukey2Message.message_type` (BAD_MESSAGE_TYPE, 2, goes out as a
CLIENT_INIT).

**Upstream fix.** `Mac::verify_slice`; refuse schemes other than
AES-256-CBC/HMAC-SHA256; send `message_type: ALERT` with the alert inside.

**Sukkula.** Patch 0013.

## R16. The device type is read from the wrong bits

**Where.** `src/hdl/inbound.rs:603`: `(endpoint_info[0] & 7) >> 1` keeps
bits 2-1; the type is bits 3-1 (`utils.rs` writes `device_type << 1`).

**Impact.** A laptop (3) is read as a phone (1); cosmetic.

**Sukkula.** Patch 0012.

## R17. Unpinned git dependencies

**Where.** `Cargo.toml:31` (`mdns-sd` from a branch of a fork, mdns-sd
0.10.4 plus two commits), `Cargo.toml:40` (`sys_metrics` from a default
branch).

**Impact.** What is built changes when the branches move; neither can be
audited as a release; the mDNS parser misses upstream's fixes since 0.10.4.

**Upstream fix.** mdns-sd from crates.io (the fork's two additions are
`register_resend`, used by the BLE path, and an address-family option);
the hostname from `gethostname`/`uname`.

**Sukkula.** Patch 0007 (mdns-sd 0.21, if-addrs 0.15 instead of the
unmaintained get_if_addrs) and 0015 (sys_metrics gone with the global
device name).

---

## Not reported, not upstream's to fix

- The generated protobuf (patch 0002) is committed only because Sukkula's
  phone build has no `protoc`; it is exactly prost-build 0.13.5's output
  with libprotoc 3.21.12 (re-generated and compared byte for byte on
  2026-09-24).
- Patches 0001, 0017 and 0018 are packaging and tests.
