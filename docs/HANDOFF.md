# Handoff: where the work stands and how to pick it up

Written 2026-09-25 so that work can resume in a new session. The
container this was built in is disposable: agent worktrees and scratch
files vanish with it. Everything needed to continue is on the branch
`claude/busy-babbage-7y67po`, in this file and in `docs/handoff/`.

## Where it stands

**Done, merged, pushed, all gates green locally** (at `8c7bd26`):

- The whole spec (`docs/SPEC.md`, v0.2): `sukkula-core` (S1–S7),
  `sukkula-engine` with all four adapters (LocalSend, Quick Share with
  `third_party/rqs_lib` = upstream 5a31145 + 18 patches, Magic Wormhole v1,
  Bluetooth OBEX), the hub and the C ABI (`sukkula-ffi`), S9 logging, the
  Qt/C++ shell and the Silica UI (en/fi/de/sv), the RPM spec, CI with the
  Harbour gate, 16 fuzz targets.
- Gates that pass on that commit: `make check` (fmt, clippy, ~400 tests,
  rustdoc, per-feature builds and tests, deps, lockfile, harbour + selftest,
  packaging, qml, cpp, cross, ffi-asan, fuzz-smoke), `make deny`,
  `make vendor`.
- `docs/SECURITY.md` (threat model, guarantees, enforcement table, clove
  comparison), `docs/UPSTREAM-QUICKSHARE.md` (drafted, not filed),
  `docs/MANUAL-TESTS.md`, `docs/FFI.md`, `docs/HARBOUR.md`,
  `docs/BUILDING.md`.

**In flight: the review-fix round.** A 94-agent adversarial review
(10 areas, every finding challenged by skeptics) kept 40 findings and
refuted 17: `docs/handoff/review-2026-09-25.json` (`kept[N]` with the
verifiers' reasoning). Six fix tasks were started, one per area
(`docs/handoff/fix-round-tasks.md`). Their committed progress is saved as
patches in `docs/handoff/wip/<area>/` (`STATE` says the base commit and
how many commits; refresh with `docs/handoff/snapshot.sh`).

| Area | State at handoff |
| --- | --- |
| qml-ui | **merged** (with engine-core) |
| engine-core | **merged** (with qml-ui); follow-ups: UI does not show `settings.recovered` yet; switching Wormhole off does not cancel a running wormhole transfer |
| localsend | **merged** (all five findings fixed and proven; F-LS1 fallback scope noted in SPEC.md) |
| wormhole-bluetooth | **merged** into the integration branch (all five findings fixed and proven; SECURITY.md S8 row and fuzz/README wording left to ci-harbour) |
| quickshare-mdns | in progress: 1 WIP commit (vendored mdns-sd 0.21.4 with bounded-cache and link-only patches); the rqs_lib host-name patch not yet |
| ci-harbour | **merged** (all 11 findings fixed and proven); the fuzz smoke then found a mailbox-guard duplicate-key disagreement, fixed on the integration branch (`strict_json`) |

All five stopped at a session usage limit on 2026-09-25 and were relaunched in the same worktrees.
This table is refreshed each time `snapshot.sh` runs and the result is
pushed; `wip/*/STATE` is the source of truth.

## How to resume

1. Check out `claude/busy-babbage-7y67po` and read this file,
   `docs/SPEC.md` and `docs/SECURITY.md`.
2. For each area in `docs/handoff/wip/`:
   - make a branch from the integration branch and `git am
     docs/handoff/wip/<area>/*.patch` (they are `git format-patch` output
     against the `base=` commit in `STATE`; if the integration branch has
     moved, `git am -3`);
   - if the area is not finished (see the table), relaunch an agent on that
     branch with `docs/handoff/brief-common.md`,
     `docs/handoff/brief-fix-round.md` and the area's section of
     `docs/handoff/fix-round-tasks.md`, telling it which findings the
     applied patches already fixed (their commit messages say);
   - when it is done, run the area's gates (listed per area) and merge.
3. Merge order: **engine-core with qml-ui** first (the settings field), then
   the others. `ci-harbour` adds `clippy.toml` bans on the dbus
   constructors that `wormhole-bluetooth` removes from the code: merge
   those two together or wormhole-bluetooth first.
4. After all six: `make check`, `make deny`, `make vendor`; then update
   `docs/SECURITY.md` with anything the fix reports flagged, delete
   `docs/handoff/` and this file, commit, push.

Practicalities learned the hard way:

- Build with `CARGO_BUILD_JOBS=2`–`3` and run `cargo clean` between big
  steps: a full build tree reaches 13–17 GB, and parallel worktrees fill a
  30 GB disk.
- The toolchain is pinned (`rust-toolchain.toml`, 1.97.1); fuzzing needs a
  nightly (`FUZZ_TOOLCHAIN=nightly`), and `cargo-deny` 0.20.2 is installed
  with `cargo +1.97.1 install cargo-deny --locked --version 0.20.2`.
- `ci/vendor-check.sh` wants the upstream sources: `git clone
  https://github.com/ignotusbucius/open-quickshare` and set
  `VENDOR_UPSTREAM_DIR` to it (or let the script fetch).
- An automated safety filter occasionally stops an agent working on
  hostile-input tests (it happened to three agents). Relaunch with the
  work framed as robustness testing of Sukkula's own receiver; the saved
  patches mean nothing is lost.

## Decisions waiting for the owner

- **Sandbox permissions.** Only `Internet;Bluetooth;Downloads` (spec §2).
  Files outside `~/Downloads` (Gallery shares, Documents) are probably
  unreadable inside Sailjail; `UserDirs` would fix it. M-8 checks it on
  the phone.
- **Sending to a PIN-protected LocalSend receiver** is not supported (the
  send command has no PIN field). M-24 records it.
- **Share-menu activation** uses `ExecDBus` in the desktop file; piirit
  does it differently. Needs a device check (M-6).
- **No pull request yet:** the remote has no base branch (only
  `claude/busy-babbage-7y67po`). Create `main` (or say which branch to
  target) and a PR can be opened.

## Not verifiable in this environment

- The SDK workflows (`sdk-image.yml`, `rpm.yml`), the `mb2` build and
  Jolla's validator on a real RPM: no Docker daemon here. CI will be the
  first run.
- Everything in `docs/MANUAL-TESTS.md`: the phone, its firewall, real
  Android/LocalSend/wormhole/Bluetooth peers.

## The 40 review findings and who owns each

| # | Severity | Status | Area | Finding | File |
| --- | --- | --- | --- | --- | --- |
| 0 | high | confirmed | quickshare-mdns | Quick Share announcement never registers: mdns-sd 0.21 refuses the hostname, so no Android phone can ever see Sukkula as a receiver (F-QS1, F-QS4) | `third_party/rqs_lib/src/hdl/mdns.rs` |
| 1 | high | unverified | quickshare-mdns | mdns-sd caches every record from every LAN host with no limit, and grows its timer heap per packet: remote memory exhaustion | `third_party/rqs_lib/src/hdl/mdns_discovery.rs` |
| 2 | medium | confirmed | engine-core | **merged** (with qml-ui); follow-ups: UI does not show `settings.recovered` yet; switching Wormhole off does not cancel a running wormhole transfer |
| 3 | medium | confirmed | localsend | **merged** (all five findings fixed and proven; F-LS1 fallback scope noted in SPEC.md) |
| 4 | medium | confirmed | qml-ui | **merged** (with engine-core) |
| 5 | medium | confirmed | qml-ui | **merged** (with engine-core) |
| 6 | medium | disputed | quickshare-mdns + localsend | Discovery peer tables fill first-come with no per-source cap, and one per-IP budget keyed on spoofable or attacker-chosen addresses also gates TLS-proven /register; no flood-then-honest test | `crates/sukkula-engine/src/quickshare/discovery.rs` |
| 7 | medium | confirmed | engine-core | **merged** (with qml-ui); follow-ups: UI does not show `settings.recovered` yet; switching Wormhole off does not cancel a running wormhole transfer |
| 8 | medium | confirmed | localsend | **merged** (all five findings fixed and proven; F-LS1 fallback scope noted in SPEC.md) |
| 9 | low | confirmed | engine-core | **merged** (with qml-ui); follow-ups: UI does not show `settings.recovered` yet; switching Wormhole off does not cancel a running wormhole transfer |
| 10 | low | confirmed | localsend | **merged** (all five findings fixed and proven; F-LS1 fallback scope noted in SPEC.md) |
| 11 | low | confirmed | localsend | **merged** (all five findings fixed and proven; F-LS1 fallback scope noted in SPEC.md) |
| 12 | low | confirmed | wormhole-bluetooth | **merged** into the integration branch (all five findings fixed and proven; SECURITY.md S8 row and fuzz/README wording left to ci-harbour) |
| 13 | low | confirmed | wormhole-bluetooth | **merged** into the integration branch (all five findings fixed and proven; SECURITY.md S8 row and fuzz/README wording left to ci-harbour) |
| 14 | low | confirmed | wormhole-bluetooth | **merged** into the integration branch (all five findings fixed and proven; SECURITY.md S8 row and fuzz/README wording left to ci-harbour) |
| 15 | low | disputed | quickshare-mdns | S7 not held on the Quick Share mDNS responder: any source is answered, legacy-unicast replies go to arbitrary addresses, and there is no per-IP rate limit | `third_party/rqs_lib/src/hdl/mdns.rs` |
| 16 | low | confirmed | wormhole-bluetooth + ci-harbour | S8 gate does not cover libdbus, which can start processes; only a runtime address check that no lint enforces | `crates/sukkula-engine/src/bluetooth/bus.rs` |
| 17 | low | confirmed | wormhole-bluetooth | **merged** into the integration branch (all five findings fixed and proven; SECURITY.md S8 row and fuzz/README wording left to ci-harbour) |
| 18 | low | disputed | engine-core | **merged** (with qml-ui); follow-ups: UI does not show `settings.recovered` yet; switching Wormhole off does not cancel a running wormhole transfer |
| 19 | low | confirmed | engine-core | **merged** (with qml-ui); follow-ups: UI does not show `settings.recovered` yet; switching Wormhole off does not cancel a running wormhole transfer |
| 20 | low | confirmed | engine-core | **merged** (with qml-ui); follow-ups: UI does not show `settings.recovered` yet; switching Wormhole off does not cancel a running wormhole transfer |
| 21 | low | confirmed | qml-ui | **merged** (with engine-core) |
| 22 | low | confirmed | qml-ui | **merged** (with engine-core) |
| 23 | low | confirmed | engine-core | **merged** (with qml-ui); follow-ups: UI does not show `settings.recovered` yet; switching Wormhole off does not cancel a running wormhole transfer |
| 24 | low | disputed | ci-harbour | **merged** (all 11 findings fixed and proven); the fuzz smoke then found a mailbox-guard duplicate-key disagreement, fixed on the integration branch (`strict_json`) |
| 25 | low | confirmed | ci-harbour | **merged** (all 11 findings fixed and proven); the fuzz smoke then found a mailbox-guard duplicate-key disagreement, fixed on the integration branch (`strict_json`) |
| 26 | low | confirmed | ci-harbour | **merged** (all 11 findings fixed and proven); the fuzz smoke then found a mailbox-guard duplicate-key disagreement, fixed on the integration branch (`strict_json`) |
| 27 | low | confirmed | ci-harbour | **merged** (all 11 findings fixed and proven); the fuzz smoke then found a mailbox-guard duplicate-key disagreement, fixed on the integration branch (`strict_json`) |
| 28 | low | confirmed | ci-harbour | **merged** (all 11 findings fixed and proven); the fuzz smoke then found a mailbox-guard duplicate-key disagreement, fixed on the integration branch (`strict_json`) |
| 29 | low | confirmed | ci-harbour | **merged** (all 11 findings fixed and proven); the fuzz smoke then found a mailbox-guard duplicate-key disagreement, fixed on the integration branch (`strict_json`) |
| 30 | low | confirmed | ci-harbour | **merged** (all 11 findings fixed and proven); the fuzz smoke then found a mailbox-guard duplicate-key disagreement, fixed on the integration branch (`strict_json`) |
| 31 | low | confirmed | ci-harbour | **merged** (all 11 findings fixed and proven); the fuzz smoke then found a mailbox-guard duplicate-key disagreement, fixed on the integration branch (`strict_json`) |
| 32 | low | confirmed | ci-harbour | **merged** (all 11 findings fixed and proven); the fuzz smoke then found a mailbox-guard duplicate-key disagreement, fixed on the integration branch (`strict_json`) |
| 33 | low | confirmed | engine-core | **merged** (with qml-ui); follow-ups: UI does not show `settings.recovered` yet; switching Wormhole off does not cancel a running wormhole transfer |
| 34 | low | confirmed | ci-harbour | **merged** (all 11 findings fixed and proven); the fuzz smoke then found a mailbox-guard duplicate-key disagreement, fixed on the integration branch (`strict_json`) |
| 35 | low | confirmed | engine-core | **merged** (with qml-ui); follow-ups: UI does not show `settings.recovered` yet; switching Wormhole off does not cancel a running wormhole transfer |
| 36 | low | confirmed | engine-core | **merged** (with qml-ui); follow-ups: UI does not show `settings.recovered` yet; switching Wormhole off does not cancel a running wormhole transfer |
| 37 | low | confirmed | engine-core + qml-ui | F-C1: Magic Wormhole cannot be disabled in Settings, and the per-protocol disable is untested at engine level | `crates/sukkula-engine/src/hub.rs` |
| 38 | low | confirmed | qml-ui | **merged** (with engine-core) |
| 39 | low | confirmed | ci-harbour | **merged** (all 11 findings fixed and proven); the fuzz smoke then found a mailbox-guard duplicate-key disagreement, fixed on the integration branch (`strict_json`) |
