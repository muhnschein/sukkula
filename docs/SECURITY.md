# Security

Sukkula accepts files from strangers. Anyone on the same Wi-Fi, anyone
within Bluetooth range, and anyone holding a wormhole code can send it
bytes, and every byte, name, size and alias they send is treated as
hostile (spec §5). This file says what Sukkula promises about that, what
enforces each promise, and how to report a way around one.

## Reporting a vulnerability

Use GitHub's **private vulnerability reporting** on this repository. It is
the only channel; do not open a public issue for a suspected
vulnerability. Include the Sukkula version or commit, the peer app and
version, what you observed, and a reproduction if you have one.

## What counts as a vulnerability

**Highest severity: anything that writes outside the inbox or before
consent.**

- A file created, overwritten, truncated, linked or deleted anywhere but
  `~/Downloads/Sukkula/` (and its `.partial/` staging directory), by any
  input from a peer.
- Any payload byte written, or buffered beyond a small fixed bound, before
  the user accepts the offer.
- An offer accepted without the user seeing it. There is no auto-accept,
  not even for known devices, and a way to get one is a vulnerability.

**Also in scope:**

- A crash, hang, or unbounded memory, CPU or descriptor use triggered by a
  peer, a mailbox or relay server, or a Bluetooth daemon's reply. Every
  parser here is a hostile-input surface by assumption.
- Spoofing in the UI: a name, alias, model or message that displays as
  something it is not (bidirectional overrides, invisible characters,
  combining-mark floods), or that is rendered as rich text or a link.
- A LAN protocol answering a peer outside private, link-local and
  unique-local address ranges (S7), or plain HTTP from LocalSend (F-LS2).
- A LocalSend send reaching a peer other than the one whose certificate
  fingerprint was announced (F-LS3).
- The TLS private key leaving `key.pem`, or file names or message text
  reaching a log at info level (S9).
- Sukkula spawning a process, opening a URL or a file, or changing Wi-Fi
  settings because of anything a peer sent (S8).

**Known limitations, stated so a report is not needed to learn them:**

- **Two consent slots are a denial-of-service surface.** At most two
  offers wait for the user (F-C3), each for up to 60 s. A LAN attacker can
  keep both busy, so an honest sender is told "busy". Per-address and
  global offer limits bound how often dialogs appear; they cannot stop
  the queue being occupied. Turning Receive off ends it.
- **Bluetooth sends name a path.** obexd, not Sukkula, opens the file,
  later and outside Sukkula's sandbox. Another process of the same user
  could swap the path in between. Size checks narrow the window; the
  obexd API cannot close it. Remote peers cannot reach this.
- **Wormhole peers choose where we connect.** After consent, a peer
  holding the code can make Sukkula open up to three outbound TCP
  connections to addresses it names (never loopback, multicast or
  unspecified), sending only fixed handshake bytes, and resolve relay host
  names it picks. Every wormhole client does the same.
- **The default wormhole mailbox is plain `ws://`.** The transfer is
  end-to-end encrypted, but anyone on the path sees the nameplate and the
  timing. A `wss://` mailbox can be configured (F-MW4).
- **magic-wormhole's reactor thread outlives the engine.** async-io starts
  it on the first wormhole transfer and cannot stop it. It holds no
  sockets once the engine has stopped and never calls into Sukkula.
- **libdbus accepts messages up to 128 MiB.** Lowering it needs unsafe
  FFI in the engine, which is forbidden. Replies from BlueZ and obexd are
  walked in place and only small copies are kept.
- **Files outside `~/Downloads` may be unreadable.** The sandbox grants
  only `Internet;Bluetooth;Downloads` (spec §2). The UI says so; this is a
  policy choice, not a bug.

**Out of scope:**

- Weaknesses in the protocols themselves (LocalSend, Quick Share, Magic
  Wormhole, OBEX) or in their reference implementations. Report those
  upstream; `docs/UPSTREAM-QUICKSHARE.md` and the module docs of
  `crates/sukkula-engine/src/{localsend,wormhole}/mod.rs` list what we
  found and worked around.
- Attacks that need code already running as the phone's user.
- What a file you chose to accept contains. Sukkula never opens a
  received file; what you open it with is that app's business.

## Design guarantees a report can hold us to

These are enforced in the code and checked in CI, not merely intended.

- **One trust boundary.** Every peer-supplied value passes through
  `sukkula-core` before anything acts on it: names (S1), display text
  (S2), sizes and counts (S4, S6), consent (S5), reach (S7). The adapters
  build a `RawOffer` from the wire and `Offer::validate` is the only way to
  an `Offer`.
- **One writer.** Only `sukkula_core::inbox` and `sukkula_core::store`
  write files, and the type system and the linter hold everyone else to
  it: a received file can only be created from a `SafeName`, and
  `clippy.toml` bans every file-writing API outside those two modules.
- **Staged, capped, verified, never clobbering.** A received file is
  written under a random name with `O_CREAT|O_EXCL`, mode `0600`, into a
  staging directory opened `O_DIRECTORY|O_NOFOLLOW` and used by
  descriptor; it never grows past its declared size; it is checked
  against the sender's SHA-256 when there is one; and it is placed with
  `linkat(2)` under a name nobody holds, never overwriting and never
  following a link. Any failure, cancel or panic deletes it.
- **Consent first, always.** No adapter reads a payload before
  `Ctx::offer` returns an accepted offer. Unanswered offers are declined
  after 60 s; a third waiting offer is declined without UI.
- **Bounded everything.** Every length is range-checked, negatives
  included, before anything is allocated. Every network read has a
  timeout. Connections, pending offers, running transfers, peers, queued
  events and in-flight commands all have hard caps (`limits.rs`).
- **The LAN is private.** LocalSend and Quick Share drop a connection from
  outside private, link-local and ULA ranges before the TLS handshake or
  the first frame, and rate-limit discovery and offers per address, with
  a global limit on offers behind it.
- **LocalSend is HTTPS-only and pinned.** No plain-HTTP listener exists.
  Sends are pinned, during the handshake, to the fingerprint the peer
  announced. Uploads are accepted only from the address and certificate
  that made the accepted offer, with the per-file token.
- **Wormhole transfer v2 is not compiled**, and the v1 path runs behind
  guards that hold the mailbox, relay and peer to size, count and shape
  limits the library does not enforce, with every library future inside
  `catch_unwind` and a timeout.
- **No side effects.** No process is spawned (`clippy.toml` bans
  `std::process::Command` and `tokio::process::Command`;
  `ci/check-deps.sh` bans process-spawning crates in the shipped graph,
  with one recorded exception: tokio's `process` module, which the LocalSend
  core's `tokio/full` compiles in and nothing calls), no shell, no URL or file is opened (the UI
  has no `Qt.openUrlExternally` and no `linkActivated`), and no Wi-Fi
  setting is touched (Quick Share's Wi-Fi Direct and hotspot upgrade is
  compiled out, credential payloads are refused).
- **Plain text only.** Every QML text item showing peer data sets
  `textFormat: Text.PlainText`, and the QML tests walk the live item tree
  to prove it.
- **`unsafe` lives in one crate.** `sukkula-core` and `sukkula-engine` are
  `#![forbid(unsafe_code)]`. `sukkula-ffi` has `unsafe` only to export
  the four C functions, read the command string and call the callback,
  each with a `SAFETY:` comment; handles are registry ids, never
  dereferenced; no panic crosses the C boundary.
- **Minimal sandbox.** Sailjail grants `Internet;Bluetooth;Downloads` and
  nothing else, and the Harbour gate fails on any other permission.

## Where each guarantee is checked

Everything below runs in CI on every pull request (`.github/workflows/ci.yml`)
and from a clean checkout with `make check`. When a row changes, change the
code or test it names.

| Guarantee | What enforces it |
| --- | --- |
| S1: a peer's name becomes one safe path component | `crates/sukkula-core/src/name.rs` tests; `tests/properties.rs` `sanitize_gives_one_safe_component`, `path_structure_never_survives`, `numbered_names_stay_safe`; `tests/hostile.rs` `names`; fuzz target `name_sanitize` (asserts the rule, against an independent oracle from the Unicode data) |
| S2: no control, bidi or invisible character reaches the screen; stacks and lengths capped | `text.rs` tests (full Cf and Default_Ignorable coverage, Unicode 16 mark table); `properties.rs` `display_keeps_its_promises`, `message_keeps_its_promises`; fuzz targets `text_display`, `text_message`; QML tests walking the item tree for `Text.PlainText`; `tests/qml/static_checks.py` |
| S3: only the inbox writes; staged, capped, verified, never clobbering or following links | `clippy.toml` bans; `inbox.rs` and `store.rs` tests (symlinked targets and staging, EXDEV copy fallback on tmpfs, cancel mid-copy, spoiled writes); `properties.rs` `the_inbox_writes_what_was_declared_or_nothing`; every adapter's hostile suite asserts nothing outside the download directory and no partial file |
| S4/S6: sizes and counts range-checked before allocation | `offer.rs` tests; `properties.rs` `validate_never_passes_a_bad_offer`, `negative_or_oversized_anywhere_is_refused`; fuzz target `offer_validate`; `localsend_hostile::impossible_sizes_and_counts_never_reach_the_user`, `oversized_json_is_refused_before_it_is_read`; `wormhole_hostile::negative_and_oversized_offers_never_reach_the_user`, `a_lying_record_length_is_refused_before_anything_is_allocated` |
| S5: consent before any payload | `consent.rs` tests; `properties.rs` `consent_accounting_holds_under_any_schedule`; `localsend_hostile::uploads_without_consent_or_the_right_token_are_refused_unread`; `localsend_loopback::a_declined_offer_writes_nothing`; `wormhole::a_declined_offer_moves_no_data_and_leaves_no_file`; `ctx::tests::a_busy_engine_declines_without_asking` |
| Every read has a timeout; slow peers are cut off | `localsend_hostile::slow_handshakes_heads_and_bodies_are_cut_off`, `a_stalled_upload_times_out_and_is_cleaned_up`, `a_receiver_that_stops_reading_times_out`; `wormhole_hostile::a_slow_loris_record_times_out`, `a_sender_that_stalls_after_the_yes_times_out`; `bluetooth::a_stalled_transfer_times_out`, `bluez_that_never_answers_times_out` |
| S7: only private LAN peers, rate-limited | `reach.rs` tests; `properties.rs` `reach_agrees_with_the_ranges`, `the_limiter_is_bounded_and_exact_below_capacity`; `ctx::tests::offers_are_limited_per_peer_and_in_total`; `localsend_hostile::unpermitted_addresses_are_dropped_before_the_handshake`, `offers_are_rate_limited_per_address` |
| F-LS2/F-LS3: HTTPS only, pinned | `localsend_hostile::plain_http_and_certificateless_clients_get_nothing`, `registrations_must_prove_their_fingerprint`; `localsend_loopback::a_changed_certificate_is_refused_before_anything_is_sent` |
| S8: no processes, no opening, no Wi-Fi changes | `clippy.toml` `disallowed-methods`/`disallowed-types`; `ci/check-deps.sh` (and its selftest) over `Cargo.lock` and the shipped aarch64 graph; `tests/qml/static_checks.py` (no `openUrlExternally`, no `linkActivated`); Quick Share patches in `third_party/rqs_lib.patches/` |
| S9: key `0600`, never logged | `store.rs` `exposed_secrets_are_refused`, symlink and owner refusal; redacted `Debug` for the PIN and the key |
| S10: `unsafe` only in `sukkula-ffi`; lints | `#![forbid(unsafe_code)]` in core and engine; workspace lints in `Cargo.toml` (`-D warnings`, no `unwrap`/`expect`/`panic`/indexing/unchecked arithmetic); overflow checks in release |
| The C boundary cannot be misused into memory unsafety | `crates/sukkula-ffi/tests/ffi.rs` (NULLs, stale handles, bad UTF-8, oversized commands, stop from the callback, hammering while stopping); `ci/ffi-harness/run.sh` under ASan, UBSan and LSan with no suppressions |
| Parsers survive hostile input | Deterministic mutation sweeps on every push: `sukkula-core/tests/hostile.rs`, `localsend_hostile::a_mutation_sweep_of_offers_breaks_nothing`, the wormhole `sweep` module, the Bluetooth reply sweeps; cargo-fuzz targets with seeds and dictionaries (`fuzz/README.md`), 60 s each per pull request |
| Harbour, sandbox and linking | `ci/harbour-check.sh` (with a 106-case selftest) on every pull request; Jolla's `rpmvalidation.sh` on the built RPM (`ci/harbour-validate-rpm.sh`); `ci/check-elf.sh` (stripped, only `main` exported, RELRO/BIND_NOW/PIE, allowed libraries only) |
| Dependencies | `cargo deny` (licences, advisories, sources, bans including a vendored libdbus); `ci/check-deps.sh` (no OpenSSL, no second TLS or D-Bus stack, no process spawning); `ci/check-lockfile.sh`; `ci/vendor-check.sh` (the vendored Quick Share library is upstream plus its reviewed patches, byte for byte) |

If you find a way to violate one of these, that is a vulnerability by
definition, even without a demonstrated exploit.

## Compared with clove

clove, the same author's I2P BitTorrent client, set the bar this project
is held to. What Sukkula shares with it:

- a guarantee-to-enforcement table like the one above, which must change
  when the code does;
- deterministic mutation sweeps on every push next to coverage-guided
  fuzz targets with dictionaries and committed seeds;
- hostile-peer suites (slow-loris, stop-reading, lying sizes, floods)
  rather than only well-formed interop tests;
- linter bans and a dependency check that make the architecture's rules
  (one writer, no processes) compile-time and CI-time facts;
- scrubbing of foreign text wherever it becomes something displayed.

Where it differs, and why:

- **No seccomp or Landlock of our own.** clove confines its daemon with a
  measured syscall allowlist. Sukkula is a Qt application in Sailjail
  (firejail), which already confines it to its permissions; a process-wide
  filter would have to cover Qt, Silica and the graphics stack, and would
  need `unsafe` outside `sukkula-ffi`. The engine's defences are
  structural instead: one writer, descriptor-relative file operations,
  bounded parsers.
- **The network boundary is per protocol, not one crate.** clove lets only
  `i2pnet` open sockets. Sukkula speaks four protocols through their own
  libraries, so each adapter owns its sockets and each is held to the same
  S7 check at accept time and the same caps.
- **Upstream code is patched or guarded, not trusted.** Where a protocol
  library could not be made to hold a rule from outside -- LocalSend's
  server (L1–L7), magic-wormhole's mailbox and transit handling (W1–W13),
  open-quickshare's file handling (Q1–Q7) -- Sukkula either drives the
  underlying libraries itself, runs the library behind a size- and
  shape-checking guard, or carries a reviewed patch, and says which in the
  module docs.
