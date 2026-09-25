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
- The TLS private key leaving `key.pem` or reaching a log at any level;
  or a file name, message text, alias, PIN, wormhole code, path in the
  download directory or peer address reaching the log at info level or
  above, with debug logging off or on (S9).
- Sukkula spawning a process, opening a URL or a file, or changing Wi-Fi
  settings because of anything a peer sent (S8).

**Known limitations, stated so a report is not needed to learn them:**

- **Two consent slots are a denial-of-service surface.** At most two
  offers wait for the user (F-C3), each for up to 60 s. A LAN attacker can
  keep both busy, so an honest sender is told "busy". Per-address and
  global offer limits bound how often dialogs appear; they cannot stop
  the queue being occupied. Turning Receive off ends it. A wormhole
  receive by code shares the two slots, but not the global limit, which
  is for offers nobody asked for: LAN offers that use it up cannot make a
  code the user typed fail as "busy".
- **Bluetooth sends name a path.** obexd, not Sukkula, opens the file,
  later and outside Sukkula's sandbox. Another process of the same user
  could swap the path in between. Size checks narrow the window; the
  obexd API cannot close it. Remote peers cannot reach this.
- **Wormhole peers choose where we connect.** After consent, a peer
  holding the code can make Sukkula try up to three outbound TCP
  connections in all to addresses it names -- one attempt per direct hint
  or relay, refused and timed-out attempts included -- never loopback,
  multicast, broadcast or unspecified, but possibly this phone's own LAN
  address; each carries only fixed handshake bytes. It can also make
  Sukkula resolve up to three relay host names it picks. Every wormhole
  client does the same, most with more attempts.
- **The default wormhole mailbox is plain `ws://`.** The transfer is
  end-to-end encrypted, but anyone on the path sees the nameplate and the
  timing. A `wss://` mailbox can be configured (F-MW4).
- **magic-wormhole's reactor thread outlives the engine.** async-io starts
  it on the first wormhole transfer and cannot stop it. It holds no
  sockets once the engine has stopped and never calls into Sukkula. A
  mailbox connection that was minting hashcash when its transfer was
  cancelled, or the engine stopped, finishes the mint (20 bits at most) on
  its own thread before that thread ends; nothing waits for it.
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
  `clippy.toml` bans every call that creates, opens for writing, links,
  renames, copies, removes or changes a file or directory -- std, tokio
  and rustix alike, the descriptor-relative `openat`/`linkat`/`unlinkat`
  family included -- outside those two modules and the `dirfd` module
  they share, where the ban is lifted one function at a time.
- **Staged, capped, verified, never clobbering.** A received file is
  written under a random name with `O_CREAT|O_EXCL`, mode `0600`, into a
  staging directory opened `O_DIRECTORY|O_NOFOLLOW` and used by
  descriptor; it never grows past its declared size; it is checked
  against the sender's SHA-256 when there is one; and it is placed with
  `linkat(2)` under a name nobody holds, never overwriting and never
  following a link. Any failure, cancel or panic deletes it. A kill, which
  runs no cleanup, leaves only staging and temporary files, which the next
  start removes: a file under its real name is always whole.
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
  that made the accepted offer, with the per-file token. The register
  fallback of discovery (F-LS1) never scans the subnet: it registers only
  with LocalSend servers it already knows of -- peers found before, peers
  that registered with us over HTTPS, announcements it could not answer
  at once -- pinned to the certificate each was known by, to permitted
  addresses, a few at a time and a few times each.
- **Wormhole transfer v2 is not compiled**, and the v1 path runs behind
  guards that hold the mailbox, relay and peer to size, count and shape
  limits the library does not enforce -- a peer message is judged in the
  place the library will read it, not by its phase alone -- with every
  library future inside `catch_unwind` and a timeout, and the mailbox
  connection, where the library mints hashcash without yielding, on a
  thread of its own rather than an engine worker.
- **Bluetooth connects only to `unix:` buses.** libdbus starts a process
  for `unixexec:` and `autolaunch:` addresses, and falls back to
  `autolaunch:` when left to find the session bus itself. Every Bluetooth
  connection is made by `bluetooth::bus::Bus::connect`, which takes the
  address from the environment or the standard socket and refuses
  anything but `unix:` right before it calls `Channel::open_private` (the
  Quick Share BLE nudge checks its system-bus address the same way); the
  dbus crate's constructors that let libdbus choose the address are never
  called.
- **No side effects.** No process is spawned. `clippy.toml` bans
  `std::process::Command` and `tokio::process::Command`, and every
  constructor of the `dbus` crate: libdbus is the one S8-relevant library
  in the build, since it runs dbus-launch for an `autolaunch:` address
  and forks and execs for a `unixexec:` one, so the ban is lifted only in
  `bluetooth::bus::Bus::connect` and the Quick Share BLE nudge, right
  after each checks that its address is a plain `unix:` socket.
  `ci/check-deps.sh` bans process-spawning and URL- or file-opening crates
  in the shipped graph, and lets no crate but the engine use `dbus`, with
  one recorded exception: tokio's `process` module, which the LocalSend
  core's `tokio/full` compiles in and nothing calls. There is no shell;
  no URL or file is opened (the UI has no `Qt.openUrlExternally` and no
  `linkActivated`); and no Wi-Fi setting is touched (Quick Share's Wi-Fi Direct and hotspot upgrade is
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
- **The log keeps quiet.** The engine logs to standard error (the
  journal), never to a file. Off by default, it says only what Sukkula's
  own crates report at warn and error; with debug logging on, Sukkula's
  crates at debug and the protocol libraries at no more than the level
  their messages were read and found clean at
  (`crates/sukkula-engine/src/logging.rs` has the table and the
  findings). Sukkula's own log lines carry counts, sizes, protocols and
  error codes, never a name, a text, an alias, a PIN, a code, a key, a
  path or an address, at any level. Every character that could forge a
  line or reorder one is escaped, and lines are capped.

## Where each guarantee is checked

Everything below runs in CI on every pull request (`.github/workflows/ci.yml`)
and from a clean checkout with `make check`. When a row changes, change the
code or test it names.

| Guarantee | What enforces it |
| --- | --- |
| S1: a peer's name becomes one safe path component | `crates/sukkula-core/src/name.rs` tests; `tests/properties.rs` `sanitize_gives_one_safe_component`, `path_structure_never_survives`, `numbered_names_stay_safe`; `tests/hostile.rs` `names`; fuzz target `name_sanitize` (asserts the rule, against an independent oracle from the Unicode data) |
| S2: no control, bidi or invisible character reaches the screen; stacks and lengths capped | `text.rs` tests (full Cf and Default_Ignorable coverage, Unicode 16 mark table); `properties.rs` `display_keeps_its_promises`, `message_keeps_its_promises`; fuzz targets `text_display`, `text_message`; QML tests walking the item tree for `Text.PlainText`; `tests/qml/static_checks.py` |
| S3: only the inbox writes; staged, capped, verified, never clobbering or following links | `clippy.toml` bans (std, tokio and rustix; lifted only in `sukkula_core::{inbox,store,dirfd}`, per function); `inbox.rs` and `store.rs` tests (symlinked targets and staging, EXDEV copy fallback on tmpfs, a file system without hard links, cancel and kill mid-copy, spoiled writes, `leftover_temporaries_are_swept_and_nothing_else`); `sukkula-core/tests/chaos.rs`, after clove's `ci/chaos.sh`: the test binary re-run as a writer and SIGKILLed 30 times per part (settings and key writes, receives placed by link, receives placed by the copy fallback across mounts), every kill checked to be what ended it and required to land mid-work, and after each the next open leaves only whole files; `properties.rs` `the_inbox_writes_what_was_declared_or_nothing`; every adapter's hostile suite asserts nothing outside the download directory and no partial file |
| S4/S6: sizes and counts range-checked before allocation | `offer.rs` tests; `properties.rs` `validate_never_passes_a_bad_offer`, `negative_or_oversized_anywhere_is_refused`; fuzz target `offer_validate`; `localsend_hostile::impossible_sizes_and_counts_never_reach_the_user`, `oversized_json_is_refused_before_it_is_read`; `wormhole_hostile::negative_and_oversized_offers_never_reach_the_user`, `a_lying_record_length_is_refused_before_anything_is_allocated` |
| S5: consent before any payload | `consent.rs` tests; `properties.rs` `consent_accounting_holds_under_any_schedule`; `localsend_hostile::uploads_without_consent_or_the_right_token_are_refused_unread`; `localsend_loopback::a_declined_offer_writes_nothing`; `wormhole::a_declined_offer_moves_no_data_and_leaves_no_file`; `ctx::tests::a_busy_engine_declines_without_asking` |
| S5 in the UI: only the user's Accept says yes, and nothing answers for them | `tests/qml/tst_consent.qml` (Decline, leaving, the countdown and an engine-closed offer all end in no, the last one unanswered); `tst_stack.qml` (a reply to a send or a code never pops or replaces a consent dialog, one dialog in the stack at a time and never a closed one left under the next, a closed offer never answered, no settings save and receiver restart under a dialog); `tst_engine.qml` (`Engine.answer` sends nothing for an offer that is not waiting); each guarded by a planted fault in `tests/qml/selftest.py` |
| Every read has a timeout; slow peers are cut off | `localsend_hostile::slow_handshakes_heads_and_bodies_are_cut_off`, `a_stalled_upload_times_out_and_is_cleaned_up`, `a_receiver_that_stops_reading_times_out`, `a_sender_that_stops_reading_is_cut_off` (32 connections held by peers that stop reading, cut off by the write deadline), `a_connection_kept_busy_is_retired`; `localsend::server::tests::a_write_that_makes_no_progress_fails`; `wormhole_hostile::a_slow_loris_record_times_out`, `a_sender_that_stalls_after_the_yes_times_out`; `bluetooth::a_stalled_transfer_times_out`, `bluez_that_never_answers_times_out` |
| S7: only private LAN peers, rate-limited | `reach.rs` tests; `properties.rs` `reach_agrees_with_the_ranges`, `the_limiter_is_bounded_and_exact_below_capacity`; `ctx::tests::offers_are_limited_per_peer_and_in_total`, `offers_the_user_asked_for_are_outside_the_global_limit`; `localsend_hostile::unpermitted_addresses_are_dropped_before_the_handshake`, `offers_are_rate_limited_per_address`, `one_address_cannot_fill_the_peer_list`; `localsend::discovery::tests::forged_announcements_cannot_hide_the_address_they_name`, `claims_naming_other_ports_cannot_use_up_localsends_own`, `a_forgery_answered_shows_the_device_it_named` (UDP announcements have a budget of their own and never spend the one TLS requests and Quick Share are held to); `localsend::peers::tests::one_address_cannot_fill_the_table`, `a_full_table_makes_room_from_the_address_holding_most`, `announced_leads_never_push_out_proven_ones` |
| F-LS2/F-LS3: HTTPS only, pinned | `localsend_hostile::plain_http_and_certificateless_clients_get_nothing`, `registrations_must_prove_their_fingerprint`, `the_fallback_sends_nothing_to_another_certificate`; `localsend_loopback::a_changed_certificate_is_refused_before_anything_is_sent`, `discovery_registers_with_peers_it_knows_without_multicast` (the F-LS1 register fallback) |
| S8: no processes, no opening, no Wi-Fi changes | `clippy.toml` `disallowed-methods`/`disallowed-types`: process spawning, and all fifteen dbus-crate connection constructors, lifted only in `Bus::connect` and the BLE nudge behind their address checks (`bus.rs` `only_unix_addresses_are_accepted`, `quickshare/ble.rs` `only_unix_addresses_are_used`, `tests/bluetooth.rs` `missing_bluez_or_system_bus_is_unavailable`); `ci/clippy-bans-selftest.sh` (every ban fires on a call it exists for, by the path it names); `ci/check-deps.sh` (and its selftest) over `Cargo.lock` and the shipped aarch64 graph: process runners, pseudo-terminals, `nix`, URL and file openers, and any crate but the engine using `dbus` (libdbus can start a process); `tests/qml/static_checks.py` (no `openUrlExternally`, no `linkActivated`); Quick Share patches in `third_party/rqs_lib.patches/` |
| S9: key `0600`, never logged | `store.rs` `exposed_secrets_are_refused`, symlink refusal, and owner refusal run unprivileged in every `cargo test` (the test-only `dirfd::theirs` makes one inode another user's to the owner check, which then runs as in the app): `store.rs` `a_file_owned_by_someone_else_is_refused`, `a_store_directory_owned_by_someone_else_is_refused`, `dirfd.rs` and `inbox.rs` `a_directory_owned_by_someone_else_is_refused`; redacted `Debug` for the PIN, the key, and an offer's text and PIN (`offer.rs` `debug_hides_the_message_and_the_pin`) |
| S9: logging off by default, to stderr only, nothing private at info or above | `logging.rs` tests (the level table off and on, a runtime switch per engine, escaping and the line cap, the `log` bridge); `hub.rs` `every_engine_thread_logs_to_the_engine_log_and_the_setting_switches_it`, `a_bad_settings_file_is_logged_by_kind_never_by_content`; `tests/s9_logs.rs`: LocalSend (wrong and right PIN, a declined offer, a file, a text), Quick Share (a file, a text, a declined offer) and wormhole (a text, a file, against a mailbox server that writes into the log) transfers with distinctive values, every event of every crate captured and judged by the engine's own table, off and on, and two engines' real log lines read back |
| F-C1: a protocol switched off in the settings is used by nothing; a settings file that does not read is never more permissive than it was | `hub.rs` `a_protocol_switched_off_in_the_settings_is_used_by_no_command` (each of the four off in turn: receive, discovery, sends, a receive by code, the device list), `a_settings_file_that_does_not_read_fails_closed_and_the_ui_is_told`; `config.rs` `one_bad_part_costs_only_that_part`, `a_bad_section_is_switched_off_not_reset_to_its_defaults`, `an_unknown_key_switches_every_protocol_off`, `a_file_that_cannot_be_read_one_way_is_locked_down`, `damage_never_opens_anything_up` |
| The UI's interface is strict and bounded | `api.rs` `unknown_fields_and_versions_are_refused`, `variants_without_fields_refuse_fields` (every command and target, field-less ones included); `hub.rs` `a_flood_of_commands_is_refused_not_queued`, `the_queue_is_bounded_and_waits_for_room`, `every_command_has_its_time_limit`, `a_switch_that_waited_long_still_finishes_and_says_so` (a receive switch is never cut off half-way), `receive_wormhole_may_wait_for_the_user_as_long_as_the_offer_does`; `ctx::tests::empty_chunks_cost_nothing_and_each_value_is_reported_once` (a peer's empty chunks put nothing on the event path) |
| S10: `unsafe` only in `sukkula-ffi`; lints | `#![forbid(unsafe_code)]` in core and engine; workspace lints in `Cargo.toml` (`-D warnings`, no `unwrap`/`expect`/`panic`/indexing/unchecked arithmetic); overflow checks in release |
| The C boundary cannot be misused into memory unsafety | `crates/sukkula-ffi/tests/ffi.rs` (NULLs, stale handles, bad UTF-8, oversized commands, stop from the callback, hammering while stopping); `ci/ffi-harness/run.sh` under ASan, UBSan and LSan with no suppressions |
| Parsers survive hostile input | Deterministic mutation sweeps on every push: `sukkula-core/tests/hostile.rs`, `localsend_hostile::a_mutation_sweep_of_offers_breaks_nothing`, the wormhole `sweep` module, the Bluetooth reply sweeps; cargo-fuzz targets with seeds and dictionaries (`ci/check-dicts.sh`, with its self-test, fails a target without either and a dictionary libFuzzer cannot parse), 60 s each per pull request at each target's own `-max_len`, the 64 KiB-capped ones from inputs at the cap and one byte past it (`scripts/fuzz-smoke.sh`, with its self-test; `fuzz/README.md`), each asserting the S-rules on what it accepts rather than only survival: the core's `name_sanitize`, `text_display`, `text_message`, `offer_validate`, `settings_json`, `hex`, `command_json`, `start_config`, and every protocol parser that reads a peer's or a server's bytes, through the adapter's own code: `localsend_prepare_upload`, `localsend_discovery`, `wormhole_wire`, `wormhole_code`, `wormhole_mailbox`, `quickshare_handshake`, `quickshare_frame` (no payload byte before consent, S5), `quickshare_mdns` |
| Harbour, sandbox and linking | `ci/harbour-check.sh` (with a 130-case selftest) on every pull request; Jolla's `rpmvalidation.sh` on the built RPM (`ci/harbour-validate-rpm.sh`), on every pull request that touches the crates, `src/`, `qml/` or the packaging (P.6); one waiver file with a namespace per check, every field matched (`ci/harbour-waivers.sh`); `ci/check-elf.sh` (stripped, only `main` exported (`--only-main`, on the packaged binary and on every pull request's probe link), RELRO/BIND_NOW/PIE, allowed libraries only; with its selftest); the SDK image pulled only by the digest `ci/sdk-image.digests` pins, and published only from `main` (`ci/packaging-lint.sh`) |
| Dependencies | `cargo deny` (licences, advisories, sources, bans including a vendored libdbus); `ci/check-deps.sh` (no OpenSSL, no second TLS or D-Bus stack, no process-spawning or opening crate, `dbus` for the engine alone); `ci/check-lockfile.sh`; `ci/vendor-check.sh` (the vendored Quick Share library is upstream plus its reviewed patches, byte for byte) |

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
