# Fix round of 2026-09-25: the six tasks

Each task hands one agent the review findings of one area
(`review-2026-09-25.json`, `kept[N]`). Every agent also reads
`brief-common.md` and `brief-fix-round.md`. To relaunch an unfinished
task, give an agent (in its own worktree, from the integration branch with
that area's `wip/` patches applied) the brief paths plus the section
below, and tell it which findings its predecessor's patches already fixed.

## quickshare-mdns: kept[0], kept[1], kept[15], Quick Share half of kept[6]

Owns `third_party/**`, `ci/vendor.conf`, `crates/sukkula-engine/src/quickshare/**`,
`crates/sukkula-engine/tests/quickshare*.rs`, `docs/UPSTREAM-QUICKSHARE.md`,
the Quick Share/mDNS rows of `docs/SECURITY.md`.

- kept[0] (high, reproduced twice): mdns-sd 0.21.4 `register()` requires a
  host name ending in `.local.`; `third_party/rqs_lib/src/hdl/mdns.rs`
  passes a bare name, so the announcement never registers and Android
  never sees Sukkula as a receiver (F-QS1/F-QS4). Fix as a new rqs_lib
  patch (0019-...), regenerate the tree (`third_party/rqs_lib.patches/check.sh`
  header), make a registration failure show as the protocol failing, and add
  a test that really registers (every Quick Share test today sets
  `mdns: false`).
- kept[1] (high, unverified): mdns-sd caches every record from any LAN host
  without a size cap, with sender TTLs, and grows its timer heap per record.
  Verify against `~/.cargo/registry/src/*/mdns-sd-0.21.4/`; if real, vendor
  mdns-sd with patches (second line in `ci/vendor.conf`) capping the cache,
  TTLs and timers, and dropping non-private sources; test the bound.
- kept[15] (low, disputed): S7 on the mDNS responder; fix alongside kept[1].
- kept[6] (medium, disputed), Quick Share part: per-source cap and
  replacement policy for the discovery peer table.
- Gates: workspace + `--features quickshare` clippy, tests,
  `ci/rqs-lib-tests.sh`, `ci/vendor-check.sh` (with
  `VENDOR_UPSTREAM_DIR` pointing at an open-quickshare clone at 5a31145)
  and its selftest, `ci/check-deps.sh`, `ci/check-lockfile.sh`,
  `cargo deny --locked check`, `ci/harbour-check.sh`.

## localsend: kept[3], kept[8], kept[10], kept[11], LocalSend half of kept[6]

Owns `crates/sukkula-engine/src/localsend/**`, `tests/localsend*.rs`,
`tests/localsend_support/**`, LocalSend rows of `docs/SECURITY.md`.

- kept[3] (medium): a client that pipelines and stops reading holds a
  connection forever (hyper's header timer is not re-armed while a response
  waits to flush); 4 addresses take all 32 slots. Add a progress/write
  deadline; server-side stop-reading test that also proves an honest peer
  still gets in.
- kept[8] (medium): F-LS1's HTTP register fallback is missing
  (`discover_at` is test-only). Implement it HTTPS-only, pinned, rate
  limited, S7-checked; register with peers already known, no subnet scans.
- kept[10] (low): resume()/release() race closes the listener while
  Receive is reported on.
- kept[11] (low): forged UDP announcements spend a victim's shared discovery
  budget and hide it (also from Quick Share).
- kept[6], LocalSend part: per-source cap for LocalSend peers.
- Gates: workspace + `--features localsend` clippy, tests; LocalSend suites
  3 times in a row.

## engine-core: kept[2], [7], [9], [18], [19], [20], [23], [33], [35], [36], engine half of [37]

Owns `crates/sukkula-core/**`, `crates/sukkula-engine/src/{hub,ctx,api,adapter,slots,lib,logging}.rs`,
`tests/{hub,hub_docs}.rs`, `crates/sukkula-ffi/**`, `docs/FFI.md`,
core/hub rows of `docs/SECURITY.md`. Keep the API source-compatible.

- kept[2]: progress throttle skipped once bytes >= total; empty writes still
  report. Report each value once; empty chunks cost nothing.
- kept[7]/[36]: owner-refusal tests only run as root; make the check
  testable unprivileged (injectable expected uid).
- kept[9]: the global offer budget must not refuse or be spent by offers the
  user asked for (wormhole receive by code).
- kept[18] (disputed): one bad field in settings.json resets everything to
  defaults, dropping the PIN and Hidden; never fall back to more permissive
  than the file.
- kept[19]: `deny_unknown_fields` not enforced for unit variants of the
  tagged `Command`/`SendTarget` enums; keep the JSON identical.
- kept[20]: COMMAND_TIMEOUT can cancel set_receiving/set_settings mid-way;
  complete or roll back, always emit a final `Receiving`.
- kept[23]/[35]: `receive_wormhole`'s consent wait runs inside the 60 s
  command timeout; give it a budget longer than handshake + consent.
- kept[33]: no kill-during-write test; sweep store temporaries on open; the
  inbox copy fallback must never leave a final-named partial; clove-style
  SIGKILL chaos test (re-exec the test binary as a child).
- kept[37], engine half: add `Settings.wormhole.enabled` (bool, default
  true, serde default); the hub honours it; engine test that disabling each
  protocol makes its commands `unavailable`. The UI uses exactly this name.
- Gates: workspace + per-feature clippy, tests, rustdoc `-D warnings`,
  `ci/ffi-harness/run.sh`.

## wormhole-bluetooth: kept[12], kept[13], kept[14], kept[16], kept[17]

Owns `crates/sukkula-engine/src/{wormhole,bluetooth}/**`,
`tests/{wormhole*,bluetooth*}.rs`, `tests/wormhole_support/**`,
`tests/fixtures/**`, Wormhole/Bluetooth rows of `docs/SECURITY.md`.

- kept[12]: the mailbox guard checks messages by phase, the library consumes
  them in arrival order, so W2 `todo!()`/W3 `split_at` stay reachable; check
  what the library will actually consume.
- kept[13]: a relay hint can make Sukkula try up to 12 peer-chosen
  addresses, not 3; bound to what SECURITY.md says.
- kept[14]: hashcash is minted on a tokio worker; move it off the runtime,
  bounded and cancellable.
- kept[16]: libdbus can autolaunch processes when constructors resolve the
  bus themselves; route everything through the one explicit-address
  constructor and list the dbus-crate constructors to ban (ci-harbour adds
  the `clippy.toml` bans).
- kept[17]: a cancel during Hello still sends CreateSession and orphans the
  obexd session.
- Gates: workspace + `--features wormhole` and `--features bluetooth`
  clippy, tests; wormhole and bluetooth suites 3 times each.

## qml-ui: kept[4], kept[5], kept[21], kept[22], kept[38], UI half of kept[37]

Owns `qml/**`, `qml-stubs/**`, `translations/**`, `tests/qml/**`,
`tests/cpp/**`, `src/**`, `docs/MANUAL-TESTS.md`, UI rows of
`docs/SECURITY.md`. Qt 5.6 only; peer strings stay `Text.PlainText`.

- kept[4]: the consent dialog opening over Settings applies the settings and
  restarts receivers, cancelling the offer shown.
- kept[5]: a second share with a Send page open leaves discovery stopped.
- kept[21]: async reply handlers pop/replace whatever page is on top.
- kept[22]: a closed consent dialog can stay under the next one; remove it
  wherever it is; never answer a closed offer.
- kept[38]: M-31 cannot fail and does not isolate the BLE nudge; the Settings
  text describes the nudge backwards (fix in en/fi/de/sv).
- kept[37], UI half: Wormhole toggle bound to `settings.wormhole.enabled`;
  hide Wormhole send/receive when off; translations.
- Gates: `tests/run-qml-tests.sh` (+ selftest faults), `ci/qml-lint.sh`,
  `tests/run-cpp-tests.sh` (both engines if `src/` changed),
  `ci/packaging-lint.sh`, `ci/harbour-check.sh`.

## ci-harbour: kept[24]–[32], kept[34], kept[39], clippy half of kept[16]

Owns `.github/**`, `ci/**` (not `ci/vendor.conf`), `scripts/**`,
`clippy.toml`, `Makefile`, `fuzz/README.md`, `docs/HARBOUR.md`,
`docs/BUILDING.md`, CI/Harbour/dependency rows of `docs/SECURITY.md`.

- kept[24] (disputed): release builds pull the SDK image by mutable tag that
  `packages: write` jobs can overwrite; pin by digest / verify.
- kept[25]: the Harbour source gate skips every `#` line; rpm.yml's PR path
  filter misses `src/` and `crates/**/*.rs`.
- kept[26]: `clippy.toml` misses `tokio::fs::OpenOptions::default`, Unix
  socket binds, rustix process-state twins; add the auto-resolving dbus
  constructors (kept[16]).
- kept[27]: check-deps' S8 denylist misses URL/file-opening and spawning
  crates.
- kept[28]: a `v*` tag on any commit publishes a release; require it on
  main.
- kept[29]: waiver matching differs between the two Harbour checks.
- kept[30]: concurrency groups keyed on bare `head_ref` let fork PRs cancel
  each other.
- kept[31]: check-elf.sh does not check "only `main` exported".
- kept[32]: fuzz-smoke passes no `-max_len`; the 64 KiB assertions can never
  fail. Per-target table.
- kept[34]: fuzz-smoke fuzzes targets without seeds/dictionary silently and
  misreports bad dictionaries; clove-style `check-dicts.sh --self-test`.
- kept[39]: wormhole interop never runs in CI; add a job with pinned Python
  magic-wormhole running the ignored interop tests; document the missing
  rquickshare interop.
- Gates: `ci/packaging-lint.sh` (shellcheck, actionlint), every
  `ci/*-selftest.sh`, `ci/harbour-check.sh`, `ci/check-deps.sh`,
  `ci/check-lockfile.sh`, `cargo deny`, workspace clippy, `make fuzz-smoke`.
