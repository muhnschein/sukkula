# Status

Where Sukkula stands against `docs/SPEC.md` (v0.2), what has been
verified and how, and what is waiting on the owner or on hardware.

## Built

Everything the spec asks for in v1.0:

- `sukkula-core` (S1–S7), the trust boundary and the only code that
  writes files.
- `sukkula-engine` with the four adapters:
  - LocalSend v2, over the upstream core's DTOs and certificate code;
  - Quick Share, over `third_party/rqs_lib` (open-quickshare 5a31145 plus
    20 patches) and `third_party/mdns-sd` (0.21.4 plus 4 patches);
  - Magic Wormhole v1;
  - Bluetooth OBEX send.
- The hub, the C ABI (`sukkula-ffi`) and per-engine logging (S9).
- The Qt/C++ shell and the Silica UI in en/fi/de/sv, with the Share menu.
- The RPM spec for the Jolla Phone 2026 (Sailfish OS 5.2+, aarch64 only).
- CI with the Harbour gate, 16 fuzz targets, the dependency policy and
  the vendor check.

## Verified

On an x86_64 host, from a clean checkout:

- `make check`: 1,123 test passes across the workspace and the
  per-protocol feature builds.
- The rest of `make check`:
  - the fuzz smoke over 16 targets;
  - the C harness under ASan/UBSan/LSan;
  - the Qt bridge and QML suites;
  - the aarch64 cross-build of the engine;
  - the Harbour source gate and its selftest;
  - packaging lint (shellcheck, actionlint).
- `make deny`.
- `make vendor`.
- `make wormhole-interop` against the pinned Python client (run during the fix round; it needs PyPI, so it is not part of `make check`).

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

- **The SDK route:** `sdk-image.yml`, `rpm.yml`, the `mb2` build and
  Jolla's validator on a real RPM. They need Docker and the Sailfish SDK
  image; the first CI run is their first run.
- **Everything in `docs/MANUAL-TESTS.md`:**
  - the phone's firewall;
  - Sailjail;
  - real Android, LocalSend, wormhole and Bluetooth peers;
  - the Share-menu activation (`ExecDBus`).
- **Tests that multicast on loopback** (Quick Share mDNS) pass here and in
  upstream's own suite, but have not yet run on GitHub runners.

## Waiting on the owner

- **Sandbox permissions.** Only `Internet;Bluetooth;Downloads` (spec §2).
  Files outside `~/Downloads`, such as Gallery shares and Documents, are
  probably unreadable in Sailjail; `UserDirs` would fix it (M-8).
- **Sending to a PIN-protected LocalSend receiver** is not supported: the
  send command has no PIN field (M-24).
- **Follow-ups from the fix round**, all small:
  - the UI does not yet show `settings.recovered`;
  - switching Wormhole off does not cancel a wormhole transfer already
    running;
  - `docs/UPSTREAM-QUICKSHARE.md` is drafted, not filed.
- **A base branch for pull requests.** The repository has none yet; only
  `claude/busy-babbage-7y67po` exists.
