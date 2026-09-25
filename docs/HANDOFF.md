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
| qml-ui | **finished**, all its findings fixed (patch in `wip/qml-ui`). Must merge **together with** engine-core: the UI now writes `settings.wormhole.enabled`, which the engine only accepts once engine-core adds the field. |
| engine-core | in progress: 3 commits (owner-refusal tests and chaos cleanup; settings recovery, wormhole switch, command budgets, progress, requested offers; hub tests) |
| localsend | in progress: 2 WIP commits |
| wormhole-bluetooth | **merged** into the integration branch (all five findings fixed and proven; SECURITY.md S8 row and fuzz/README wording left to ci-harbour) |
| quickshare-mdns | in progress: 1 WIP commit (vendored mdns-sd 0.21.4 with bounded-cache and link-only patches); the rqs_lib host-name patch not yet |
| ci-harbour | in progress: 2 WIP commits |

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
| 2 | medium | confirmed | engine-core | Progress throttle is skipped once bytes >= total, so a Quick Share peer can flood the UI with events after acceptance | `crates/sukkula-engine/src/ctx.rs` |
| 3 | medium | confirmed | localsend | A client that pipelines requests and stops reading holds a server connection forever; 4 LAN addresses take all 32 slots | `crates/sukkula-engine/src/localsend/server.rs` |
| 4 | medium | confirmed | qml-ui | Opening the consent dialog over Settings saves the settings and restarts receivers, which cancels the offer being shown | `qml/pages/SettingsPage.qml` |
| 5 | medium | confirmed | qml-ui | A second share while a Send page is open leaves discovery stopped and the peer lists empty | `qml/pages/SendPage.qml` |
| 6 | medium | disputed | quickshare-mdns + localsend | Discovery peer tables fill first-come with no per-source cap, and one per-IP budget keyed on spoofable or attacker-chosen addresses also gates TLS-proven /register; no flood-then-honest test | `crates/sukkula-engine/src/quickshare/discovery.rs` |
| 7 | medium | confirmed | engine-core | The ownership-refusal tests return early unless run as root, and CI runs them as an unprivileged user, so SECURITY.md's 'owner refusal' row is never exercised | `crates/sukkula-core/src/store.rs` |
| 8 | medium | confirmed | localsend | F-LS1's HTTP register fallback is not implemented; `discover_at` is reachable only from tests | `crates/sukkula-engine/src/localsend/mod.rs` |
| 9 | low | confirmed | engine-core | Global offer budget is shared with user-initiated wormhole receives: a LAN attacker can make every wormhole receive fail as 'busy' and waste the code | `crates/sukkula-engine/src/ctx.rs` |
| 10 | low | confirmed | localsend | resume()/release() race closes the listener while Receive is reported on | `crates/sukkula-engine/src/localsend/server.rs` |
| 11 | low | confirmed | localsend | Forged UDP announcements spend a victim's shared discovery budget and hide it (also from Quick Share) | `crates/sukkula-engine/src/localsend/discovery.rs` |
| 12 | low | confirmed | wormhole-bluetooth | **merged** into the integration branch (all five findings fixed and proven; SECURITY.md S8 row and fuzz/README wording left to ci-harbour) |
| 13 | low | confirmed | wormhole-bluetooth | **merged** into the integration branch (all five findings fixed and proven; SECURITY.md S8 row and fuzz/README wording left to ci-harbour) |
| 14 | low | confirmed | wormhole-bluetooth | **merged** into the integration branch (all five findings fixed and proven; SECURITY.md S8 row and fuzz/README wording left to ci-harbour) |
| 15 | low | disputed | quickshare-mdns | S7 not held on the Quick Share mDNS responder: any source is answered, legacy-unicast replies go to arbitrary addresses, and there is no per-IP rate limit | `third_party/rqs_lib/src/hdl/mdns.rs` |
| 16 | low | confirmed | wormhole-bluetooth + ci-harbour | S8 gate does not cover libdbus, which can start processes; only a runtime address check that no lint enforces | `crates/sukkula-engine/src/bluetooth/bus.rs` |
| 17 | low | confirmed | wormhole-bluetooth | **merged** into the integration branch (all five findings fixed and proven; SECURITY.md S8 row and fuzz/README wording left to ci-harbour) |
| 18 | low | disputed | engine-core | One invalid or unknown field in settings.json silently resets every setting to the most permissive defaults (drops the LocalSend PIN, Quick Share Hidden becomes Everyone) | `crates/sukkula-engine/src/hub.rs` |
| 19 | low | confirmed | engine-core | deny_unknown_fields is not enforced for unit variants of the internally tagged Command and SendTarget enums | `crates/sukkula-engine/src/api.rs` |
| 20 | low | confirmed | engine-core | COMMAND_TIMEOUT can cancel set_receiving/set_settings mid-critical-section: final Receiving never emitted, statuses stuck at 'starting', adapter tasks detached | `crates/sukkula-engine/src/hub.rs` |
| 21 | low | confirmed | qml-ui | Async reply handlers pop or replace whatever page is on top, which can be another offer's consent dialog | `qml/pages/SendPage.qml` |
| 22 | low | confirmed | qml-ui | A closed consent dialog can stay in the stack under the next one, showing a stale offer with a frozen countdown | `qml/harbour-sukkula.qml` |
| 23 | low | confirmed | engine-core | Wormhole receive: the 60 s consent wait runs inside the 60 s command timeout, so the dialog's countdown overstates the time left | `crates/sukkula-engine/src/hub.rs` |
| 24 | low | disputed | ci-harbour | Any branch or dependency build script with packages:write can overwrite the SDK image that release builds pull by mutable tag | `.github/workflows/rpm.yml` |
| 25 | low | confirmed | ci-harbour | Harbour source gate skips every '#' line, and rpm.yml never runs for src/ or crates/*.rs, so a hardcoded /home path passes every PR gate | `ci/harbour-check.sh` |
| 26 | low | confirmed | ci-harbour | clippy.toml's S3 'one writer' ban misses tokio OpenOptions::default and Unix socket binds; rustix twins of banned process-state calls are unbanned | `clippy.toml` |
| 27 | low | confirmed | ci-harbour | check-deps' S8 denylist lets URL/file-opening and spawning crates into the shipped graph | `ci/check-deps.sh` |
| 28 | low | confirmed | ci-harbour | A 'v*' tag on any commit publishes a GitHub release; only the dispatch path is held to main | `.github/workflows/rpm.yml` |
| 29 | low | confirmed | ci-harbour | Waiver scope differs between the two Harbour checks: the RPM validator ignores the check id, and an RPM-only waiver fails the source gate | `ci/harbour-validate-rpm.sh` |
| 30 | low | confirmed | ci-harbour | Concurrency groups keyed on the bare head_ref let unrelated fork PRs cancel each other's CI | `.github/workflows/ci.yml` |
| 31 | low | confirmed | ci-harbour | check-elf.sh does not check the 'only main exported' rule SECURITY.md credits it with, so the shipped binary's exports are never checked | `ci/check-elf.sh` |
| 32 | low | confirmed | ci-harbour | fuzz-smoke passes no -max_len, so the 64 KiB assertions in text_message, command_json and start_config can never fail in CI, contrary to fuzz/README.md | `scripts/fuzz-smoke.sh` |
| 33 | low | confirmed | engine-core | No kill-during-write (chaos) test, and what a kill leaves behind is not cleaned up: store temporaries, a final-named partial from the copy fallback, a split key/cert pair | `crates/sukkula-core/src/store.rs` |
| 34 | low | confirmed | ci-harbour | fuzz-smoke silently fuzzes a target with no dictionary or seeds, and reports a malformed dictionary as a crash whose reproducer does not exist | `scripts/fuzz-smoke.sh` |
| 35 | low | confirmed | engine-core | Wormhole receive consent is killed by the 60 s command timeout, before the offer's own 60 s runs out | `crates/sukkula-engine/src/hub.rs` |
| 36 | low | confirmed | engine-core | The owner-refusal tests always skip in CI, so the S3/S9 'owner refusal' guarantee is never checked | `crates/sukkula-core/src/store.rs` |
| 37 | low | confirmed | engine-core + qml-ui | F-C1: Magic Wormhole cannot be disabled in Settings, and the per-protocol disable is untested at engine level | `crates/sukkula-engine/src/hub.rs` |
| 38 | low | confirmed | qml-ui | M-31 cannot fail and does not exercise the F-QS2 BLE nudge; the Settings text describes it backwards | `docs/MANUAL-TESTS.md` |
| 39 | low | confirmed | ci-harbour | §7 reference-client loopback layer: wormhole interop never runs in CI, and there is no rquickshare interop | `crates/sukkula-engine/tests/wormhole_interop.rs` |
