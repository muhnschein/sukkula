# Shared brief (every Sukkula agent)

You are one of eight agents building **Sukkula** in parallel, each in its own git worktree of the same repo, all starting from the skeleton commit. Sukkula is a GPL-3.0-or-later Sailfish OS app (Harbour package `harbour-sukkula`) that sends and receives files and text over LocalSend v2, Quick Share, Magic Wormhole v1 and Bluetooth OBEX from one Silica UI.

## Read first
- `docs/SPEC.md` — the specification (v0.2). Requirement IDs F-*, S1–S10 are referenced everywhere. Follow it; where it is silent, choose the most secure option and say so in your report.
- `crates/sukkula-core/src/*.rs` — the trust boundary: `name` (S1), `text` (S2), `inbox`/`store` (S3), `offer`+`limits` (S4/S6), `consent` (S5), `reach` (S7), `config` (settings).
- `crates/sukkula-engine/src/{api.rs,ctx.rs,adapter.rs,hub.rs}` — the JSON contract, the shared receive path every adapter follows, the `Adapter` trait, and the hub.
- `crates/sukkula-ffi/include/sukkula.h` — the C ABI.

## Hard constraints from the owner
- Target is the **Jolla Phone 2026 only**: Sailfish OS 5.2+, **aarch64 only**. No armv7hl, no i486, no compatibility code or baggage for older OS releases or devices.
- **Harbour only**: no OpenRepos, no Chum, nothing that exists only for them. Everything must pass Jolla's Harbour validator (`sfdk check -s harbour`); Harbour rules are CI gates.
- **Be paranoid: all input is hostile.** Every byte, name, size, alias, frame, JSON field from a peer is attacker-controlled. No crash, hang, unbounded allocation or file write outside the inbox from any input. Bound every loop, buffer, queue, timeout.
- Compare against the owner's security reference project **clove** (checked out read-only at `/home/user/muhnschein/clove`): its `SECURITY.md` guarantee→enforcement table, `crates/clove-core/tests/hostile.rs` (deterministic mutation sweep), `tests/evil_peer.rs` (slow-loris, stop-reading, lying peers), `clippy.toml` bans, `ci/check-net-deps.sh`, fuzz targets with dictionaries and committed seeds. Match that rigour in your area.
- Sibling projects by the same owner with Sailfish/Harbour experience, read-only: `/home/user/muhnschein/piirit` (Delta Chat client: `ci/harbour-check.sh`, `ci/harbour/*.conf`, `docs/HARBOUR.md`, `docs/BUILDING.md`, `.github/workflows/{ci,rpm,sdk-image}.yml`, `ci/build-sdk-image.sh`, `qml/`, share-menu integration in `qml/share/` and `harbour-piirit.desktop`) and `/home/user/muhnschein/vuo` (Miniflux reader: `scripts/cross-build.sh` cross-compiles Rust with host cargo + the SDK's aarch64 GCC and sysroot, `docs/sdk-build.md`, `docs/packaging.md`, `qml-stubs/`). Reuse their proven solutions and conventions (commenting style: explain *why*, cite spec IDs).
- Upstream sources, read-only: `/home/user/upstream/localsend` (LocalSend core crate is `packages/core`, pinned rev e768240), `/home/user/upstream/open-quickshare` (rqs_lib is `core_lib`, commit 5a31145), `/home/user/upstream/magic-wormhole.rs` (0.8.1).

## Code rules
- Rust toolchain pinned to 1.97.1 (`rust-toolchain.toml`, already installed with the aarch64 target). Edition 2024.
- Workspace lints (Cargo.toml) deny warnings, `unwrap`/`expect`/`panic`/`todo`/`unreachable`, indexing/slicing, unchecked arithmetic, lossy `as` casts, print macros. Tests may unwrap/index/panic (clippy.toml allows it in `#[cfg(test)]`); integration test files under `tests/` need `#![allow(clippy::arithmetic_side_effects, ...)]` etc. at the top as needed. `clippy.toml` bans process spawning (S8) and all file writing outside `sukkula_core::{inbox,store}` (S3). Do not weaken lints or bans; if one truly must be lifted, lift it at the single call site with a comment.
- `unsafe` only in `sukkula-ffi`. `sukkula-core` and `sukkula-engine` are `#![forbid(unsafe_code)]`.
- No new async runtime, TLS stack or D-Bus stack beyond what the spec lists (tokio; smol/async-io only because magic-wormhole brings it; rustls+ring via LocalSend; the `dbus` crate over the system libdbus-1). Never `vendored` libdbus. Every new dependency needs a one-line reason in your report.
- Linked system libraries must stay within Harbour's allow-list (Qt5 Core/Gui/Qml/Quick/DBus, libsailfishapp, libdbus-1.so.3, libz, libc/libm/libdl/libpthread/libgcc_s/libstdc++). No OpenSSL.
- Tests must run offline on an x86_64 Linux host (`cargo test`), deterministic, fast (< ~60 s per test binary), no real network beyond loopback, no real Bluetooth. Loopback peers are allowed only via `StartConfig.allow_loopback` / `ReachPolicy { allow_loopback: true }`.
- `cargo fmt --all`, `cargo clippy --workspace --all-targets -- -D warnings` (plus with only your feature: `-p sukkula-engine --no-default-features --features <yours>`), and `cargo test` must be clean in your worktree before you finish.
- Machine has 4 cores shared by 8 agents: export `CARGO_BUILD_JOBS=2` for every cargo command. Keep disk use modest; do not build release unless your task needs it.

## Ownership (avoid merge conflicts)
Edit only the files your task owns. Shared contract files (`crates/sukkula-engine/src/{api.rs,ctx.rs,adapter.rs,hub.rs,lib.rs}`, `crates/sukkula-core/**`, `Cargo.toml` at the root) belong to specific agents; if you believe a contract change is required, make the smallest additive change, mark it with a `// CONTRACT:` comment, and list it prominently in your report so the integrator can reconcile. Adding dependencies to `crates/sukkula-engine/Cargo.toml` under *your* feature is fine. `Cargo.lock` will be regenerated at merge; commit yours anyway.

## Finish
- Commit all your work in your worktree with clear messages (normal prose commit messages, no model names). End every commit message with the two trailer lines:
  `Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>`
  `Claude-Session: https://claude.ai/code/session_01ENZSCiAud5Vs29ugEacpyR`
  Do NOT push. Do not create branches other than the worktree's own.
- Your final message is a report for the integrator: branch name and commit ids, what you built (by spec ID), what you verified and how (exact commands and results), what is NOT done or not verifiable here (e.g. needs a real phone), every contract change, every new dependency with its reason, and every security concern you found anywhere (even outside your area).
