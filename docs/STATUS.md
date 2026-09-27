# Status

Where Sukkula stands against `docs/SPEC.md` (v0.5), what has been
verified and how, and what is waiting on the owner or on hardware.

## Built

Everything the spec asks for in v1.0:

- `sukkula-core` (S1–S7), the trust boundary and the only code that
  writes files.
- `sukkula-engine` with the five adapters:
  - LocalSend v2, over the upstream core's DTOs and certificate code;
  - Quick Share, over `third_party/rqs_lib` (open-quickshare 5a31145 plus
    20 patches) and `third_party/mdns-sd` (0.21.4 plus 4 patches);
  - Magic Wormhole v1;
  - croc 11, our own implementation of its protocol, and croc 10's for
    its clients still about (spec v0.5);
  - Bluetooth OBEX send.
- The hub, the C ABI (`sukkula-ffi`) and per-engine logging (S9).
- The Qt/C++ shell and the Silica UI in en/fi/de/sv, with the Share menu
  and Send | Receive as the main page's two tabs (spec v0.5): the send
  radar puts every protocol's peers on one screen, the receive radar
  shows who is sending, and History lists what went and came.
- The RPM spec for the Jolla Phone 2026 (Sailfish OS 5.2+, aarch64 only).
- CI with the Harbour gate, 21 fuzz targets, the dependency policy,
  the vendor check, and interop against the Python wormhole client and
  croc's Go binaries, 11 and 10.

## Verified

On an x86_64 host, from a clean checkout:

- `make check`: 1,123 test passes across the workspace and the
  per-protocol feature builds.
- The rest of `make check`:
  - the fuzz smoke over 16 targets (the five croc targets since: 60 s
    each, no finding in the engine);
  - the C harness under ASan/UBSan/LSan;
  - the Qt bridge and QML suites;
  - the aarch64 cross-build of the engine;
  - the Harbour source gate and its selftest;
  - packaging lint (shellcheck, actionlint).
- `make deny`.
- `make vendor`.
- `make wormhole-interop` against the pinned Python client (run during the fix round; it needs PyPI, so it is not part of `make check`).
- croc's interop tests, all nine, against croc v11.5.4's Go binary with croc v10.7.0's as the other peer on its relay, and against croc v10.7.0's alone, on the development container (spec v0.5).

On GitHub, in pull request #2 (the first runs of `ci.yml` and `rpm.yml`):

- every `ci.yml` job has run, the multicast-on-loopback tests passing.
  What the runners found, all in tests and CI scripts, is fixed: rustc's
  native-libs note parsed through ANSI colour, upstream mdns-sd's IPv6
  test (which this sandbox has to skip) against patch 0001's reply limit,
  and a consent race test whose deadline was shorter than tokio's timer
  tick;
- `rpm.yml`: the SDK image derived, the engine cross-built, the RPM built
  by `mb2`, and Jolla's validator accepting it with no findings. Our own
  `ci/check-elf.sh` then refused the RPATH the SDK's `sailfishapp` feature
  adds; `src/hardening.pri` now keeps it out of the link.

On the Jolla Phone 2026, from pull request #2:

- **The first RPM** installed and its UI came up, but the engine failed
  internally: tokio panicked entering its runtime. The GL stack (Android's,
  under libhybris) keeps its thread-locals at the thread pointer, where
  glibc had put the engine's.
- **The second** had the engine as a private library,
  `libsukkula_ffi.so` (spec v0.3), which is needed because the app grid's
  booster `dlopen()`s the binary. It also put a 48-byte marker at `tp+16`.
  That RPM died in the GL stack on every start (grid, `sailjail`, Share
  menu, plain). libhybris places the Android libraries' thread-locals from
  `tp+0` on and never initialises them, so Android code read the marker
  as its own state.
- **The third** replaces the marker with a 4096-byte reserve of zeros
  (`docs/FFI.md`, Linking). `ci/hybris-tls-test.sh` reproduces each
  failure under qemu-aarch64 and shows each fix. It has not been on the
  phone yet; `SUKKULA_TLS_REPORT=1` prints how much of the reserve the GL
  stack used there.

`sukkula-core` line coverage was 98 % when last measured.

An adversarial review on 2026-09-25 covered 10 areas, with every finding
challenged by skeptical second reviewers:

- 40 findings were kept and 17 refuted.
- All 40 kept findings are fixed. Each fix has a regression test that was
  shown to fail without it, or a manual test ID where only a phone can
  answer.

The findings are summarised by area in `docs/SECURITY.md` and the module
docs; the upstream ones are in `docs/UPSTREAM-QUICKSHARE.md`.

## Not verified here

- **`sdk-image.yml`**, which runs from `main` only. Until it has run and
  its digest is pinned, every `rpm.yml` run derives the SDK image itself
  (about 7 minutes).
- **Everything in `docs/MANUAL-TESTS.md`:**
  - the phone's firewall;
  - Sailjail;
  - real Android, LocalSend, wormhole, croc and Bluetooth peers;
  - the Share-menu activation (`ExecDBus`);
  - how the radars and the tabs look and feel on the phone (M-18, M-19):
    the host tests lay them out but never look at them or swipe.

## Waiting on the owner

- **Sending to a PIN-protected LocalSend receiver** is not supported: the
  send command has no PIN field (M-24).
- **Follow-ups from the fix round**, all small:
  - the UI does not yet show `settings.recovered`;
  - switching Wormhole off does not cancel a wormhole transfer already
    running;
  - `docs/UPSTREAM-QUICKSHARE.md` is drafted, not filed.
- **The default branch.** `main` exists now; making it the repository's
  default (Settings, General) is the owner's. Until then Dependabot and
  `workflow_dispatch` keep using the old branch.

## Later

Wanted, and not in v1.0 because each needs a Sailjail permission that
spec §2 does not grant. Either is a spec change: the permission goes into
§2, `harbour-sukkula.desktop` and `POLICY_PERMISSIONS` in
`ci/harbour-check.sh` in one commit, never a workaround.

- **Scanning a wormhole code as a QR code** (F-MW2). Needs `Camera`, a
  camera view in QML and a QR decoder. The decoder reads what the lens
  sees, so it is hostile input: a new dependency with its own review
  (spec §6), a fuzz target, and the decoded text through the same
  `code::parse` a typed code goes through.
