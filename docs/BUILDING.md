# Building, testing and packaging

How to build and check Sukkula on a host, how the engine is cross-compiled
for the phone, and how the device RPM is made. The CI side of each step is
`.github/workflows/ci.yml` (the per-pull-request gate) and `rpm.yml` (the
package); Harbour's rules are `docs/HARBOUR.md`.

## The shape of the build (spec §3)

```
rust-toolchain.toml (1.97.1)          Sailfish SDK 5.2.0.15, aarch64
        |                                      |
  host cargo  --CC/AR/sysroot-->  SDK aarch64 GCC 10 + target sysroot
        |                                      |
  target/aarch64-unknown-linux-gnu/release/libsukkula_ffi.a
                                               |
         SDK aarch64 GCC: -shared, the export map  -->  libsukkula_ffi.so
                                               |
                         mb2: qmake harbour-sukkula.pro, make, strip
                                               |
                        RPMS/harbour-sukkula-<v>-<r>.aarch64.rpm
                                               |
                      ci/check-elf.sh + Jolla's rpmvalidation.sh
```

- The **engine** is the Rust workspace (`crates/sukkula-{core,engine,ffi}`,
  with `third_party/rqs_lib` vendored). `sukkula-ffi` builds the static
  library the shell links, whose C ABI is `crates/sukkula-ffi/include/sukkula.h`.
- The **shell** is the Qt/C++ start-up code and bridge (`src/`), the Silica
  UI (`qml/`), `translations/`, `icons/` and `harbour-sukkula.desktop`,
  built by qmake from `harbour-sukkula.pro` inside the SDK, from
  `rpm/harbour-sukkula.spec`.

## Toolchains

**Rust 1.97.1**, pinned by `rust-toolchain.toml` (the LocalSend core's own
pin), with rustfmt, clippy and the `aarch64-unknown-linux-gnu` std. rustup
installs it on the first cargo call in the tree. Pinned rather than
`stable` because clippy drift turns `-D warnings` into a lottery.

**The Sailfish SDK's Rust is never used.** It ships 1.75; magic-wormhole
0.8 needs 1.92 and open-quickshare's rqs_lib is edition 2024. Harbour
validates the RPM, not the compiler that made it, so the engine is
compiled by the pinned toolchain, driving the SDK's own aarch64 GCC against
the SDK's target sysroot -- the two parts that have to match the phone
(vuo's route; its sdk-build notes have the history). There is no MSRV job
and no lockfile-format constraint for the SDK's cargo, because nothing
here is built by it.

**A pinned nightly, `nightly-2026-09-15`**, for the fuzzers only
(`SUKKULA_NIGHTLY` in `ci.yml` and the Makefile, `FUZZ_TOOLCHAIN` for
`scripts/fuzz-smoke.sh`). Pinned so a nightly regression is a deliberate
bump rather than a red pull request.

Host packages (Ubuntu 24.04):

```sh
sudo apt install dbus libdbus-1-dev pkg-config \
    gcc-aarch64-linux-gnu g++-aarch64-linux-gnu binutils-aarch64-linux-gnu \
    qtbase5-dev qtdeclarative5-dev qtdeclarative5-dev-tools qml-module-qtquick2 \
    qml-module-qttest qml-module-qtquick-window2 qml-module-qtquick-layouts \
    qttools5-dev-tools rpm file binutils desktop-file-utils shellcheck
cargo install --locked cargo-fuzz cargo-deny
pip install actionlint-py==1.7.12.25
rustup toolchain install nightly-2026-09-15 --profile minimal
```

`libdbus-1-dev` is for the `bluetooth` feature: the `dbus` crate links the
system libdbus-1 through pkg-config, and is never built `vendored` (Q7).
`dbus` is for its tests, which run a private `dbus-daemon` with a fake
BlueZ on it. The FFI harness needs a C compiler that links sanitized
programs; Ubuntu's gcc does, with its own libasan and libubsan.

## make check is CI

`make check` runs exactly what `ci.yml` runs on a pull request, job for
job, minus the three that need the network rather than this tree: `make
deny` (the RustSec advisory database), `make vendor` (open-quickshare at
the pinned commit) and `make wormhole-interop` (the Python client, from
PyPI). A missing tool fails it, as CI's strict modes do: a
green that skipped the QML or the cross build is a green that means
nothing.

| `make` | CI job | What it gates |
|---|---|---|
| `fmt-check lint test doc` | `test` | rustfmt; clippy over the workspace and its tests, warnings denied, then the proof that `clippy.toml`'s bans fire (`ci/clippy-bans-selftest.sh`: a throwaway crate calls each banned API and must be told off by the path it is banned under); every test; rustdoc with broken links as errors. `--locked` throughout |
| `features` | `features` | each protocol feature alone, and none, clippy and tests (spec §3) |
| `deny` | `deny` | licences, advisories, duplicate versions, banned crates and features, sources (`deny.toml`) |
| `deps lockfile` | `deps` | the dependency budget (`ci/check-deps.sh`, with its selftest) and the lockfile rules (`ci/check-lockfile.sh`) |
| `rqs-lib-tests` | `test` | the vendored Quick Share library's own tests, run from a temporary copy so `third_party/` stays byte-identical to upstream plus patches (`ci/rqs-lib-tests.sh`) |
| `fuzz-lint fuzz-smoke` | `fuzz-smoke` | every target's seeds and dictionary (`ci/check-dicts.sh`, and its self-test), clippy and the harness tests of the fuzz crate, then every cargo-fuzz target, 60 s each, from its committed seeds, at its own `-max_len` (`fuzz/README.md`) |
| `ffi-asan` | `ffi-asan` | the C harness over the C ABI under AddressSanitizer (`ci/ffi-harness/run.sh`) |
| `cross` | `cross` | the aarch64 engine, with a probe linked against it (below) |
| `qml` | `qml` | qmllint over `qml/` and `qml-stubs/`; the UI's QML tests, offscreen |
| `cpp` | `cpp` | the Qt bridge's tests under ASan and UBSan, the shell booted offscreen, and the ELF and install-layout checks of `harbour-sukkula.pro`, against the stub engine and then the real one (`tests/run-cpp-tests.sh`) |
| `packaging` | `packaging` | spec parses and builds out of tree, desktop entry, catalogs, shellcheck on every script, actionlint on every workflow; `scripts/sonar-report.sh` against a stub server (`ci/sonar-report-selftest.sh`) |
| `vendor` | `vendor` | `third_party/rqs_lib` is upstream plus its patches (with the checker's selftest) |
| `wormhole-interop` | `wormhole-interop` | Sukkula against the Python magic-wormhole client, both ways (below, "Interop with the reference clients") |
| `harbour` | `harbour` | the source-level Harbour gate, then its selftest (`docs/HARBOUR.md`) |
| `rpm` | `rpm.yml` | the device RPM, its binary rules and Jolla's validator |

The first run fetches the pinned toolchains through rustup, which is
network; nothing after that is, until `deny`, `vendor` or
`wormhole-interop`.

## Tests

`cargo test --workspace` runs offline on an x86_64 host, deterministic, with
no network beyond loopback and no Bluetooth. Loopback peers exist only
through `StartConfig.allow_loopback` (spec S7). Each protocol builds and
tests alone:

```sh
cargo test -p sukkula-engine --no-default-features --features wormhole
```

## Cross-building the engine

`scripts/cross-build-rust.sh` has two routes, and both leave the library
where the qmake project and the spec expect it,
`target/aarch64-unknown-linux-gnu/release/libsukkula_ffi.so`. cargo builds
the static archive beside it, and the script links that into the shared
library with the same compiler, exporting the C ABI alone
(`crates/sukkula-ffi/exports.map`; `docs/FFI.md`, Linking, says why a
library).

**`--sdk <target-sysroot>`, the release route.** Every package is built
this way. The SDK's `aarch64-meego-linux-gnu-gcc` (GCC 10) compiles every
C file in the graph -- ring's above all -- against the Sailfish OS 5.2
target's own glibc headers, with the hardening the SDK's own C++ gets
(`-O2 -D_FORTIFY_SOURCE=2 -fstack-protector-strong -fstack-clash-protection
-mbranch-protection=standard -fPIC`), and libdbus-sys finds the target's
libdbus-1 through a pkg-config pointed at the sysroot and nowhere else. What
it needs, and why (vuo found each the hard way):

- the SDK's `/opt/cross` **at that absolute path**: GCC resolves `cc1`, its
  specs and its libexec against its own install prefix;
- a directory of unprefixed binutils passed as `-B`, or GCC runs the host's
  x86 `as` and dies with "unrecognized option '-EL'";
- the 32-bit x86 runtime (`libc6:i386 libstdc++6:i386 zlib1g:i386`), since
  the SDK's compilers are i386 programs, and the SDK tooling's own `libmpc`,
  `libmpfr` and `libgmp`, which `cc1` loads -- `SFOS_CROSS_LIBS` names
  their directory, reaching the compiler through a wrapper rather than
  through `LD_LIBRARY_PATH` on everything cargo runs;
- `--sysroot` on every compile and link.

`scripts/build-rpm.sh lift <image> <dir>` copies exactly those out of the
SDK image, and `scripts/build-rpm.sh engine <dir>` runs this route against
them.

**`--host`, the per-pull-request smoke.** Ubuntu's `aarch64-linux-gnu-gcc`
and its sysroot. It proves the graph cross-compiles and links for aarch64,
but Ubuntu's glibc is newer than the phone's, so nothing built this way is
ever packaged. Where no arm64 libdbus-1 is installed, the probe links
against a stand-in built on the spot with libdbus-1's soname and its
`LIBDBUS_1_3` version node, carrying exactly the `dbus_*` symbols the
engine needs.

After either build the script proves the result, and fails on any of it:

1. the pinned rustc built it (refusing any other, the SDK's included);
2. the archive exports the four entry points of `sukkula.h`;
3. every native library rustc says it needs is classified against Harbour's
   allowed list -- `-lutil`, which Rust's libc crate names for `openpty`
   and nothing here calls, is not, and must never reach `NEEDED` (against
   glibc 2.34+ it resolves to an empty compatibility archive, and the shell
   links with `--as-needed`);
4. a **probe** -- a C program calling all four entry points -- links against
   it the way the shell does (PIE, RELRO, BIND_NOW, `--as-needed`), and
   `ci/check-elf.sh` reads it back: every `NEEDED` library allowed, no
   glibc symbol version above the ceiling (the sysroot's own glibc with
   `--sdk`; 2.34 with `--host`, the version Harbour's
   `__libc_start_main@GLIBC_2.34` rule guarantees every accepted phone
   has), `__libc_start_main@GLIBC_2.34` linked, and the hardening present.

## The SDK container

`rpm.yml` builds in a Sailfish SDK image that this repository derives and
publishes to its registry (`ci/build-sdk-image.sh`, `sdk-image.yml`),
adapted from piirit's:

- from `coderus/sailfishos-platform-sdk:5.2.0.15` **by digest** -- a third
  party's image, run privileged with the checkout mounted read-write;
- one architecture's targets kept (aarch64, pristine and the `.default`
  snapshot mb2 builds in), the others deleted, and the result flattened
  with `docker export | docker import` so the deletions are real;
- the spec's BuildRequires installed into the snapshot, and checked there
  one by one -- including `pkgconfig(dbus-1)`, because the snapshot is also
  the sysroot the engine is compiled against.

### The SDK image

The image's name carries a hash of `ci/build-sdk-image.sh` and the spec's
BuildRequires (`scripts/build-rpm.sh image`), so a change to either wants
an image of its own. But a name is only a name: a registry tag is a
pointer that anyone who can push to the registry can move, and this image
compiles, links and packages every release. So nothing pulls it by its
tag. `ci/sdk-image.digests` pins each tag to a digest, reviewed like code,
and `scripts/build-rpm.sh pull` pulls `<registry>/sukkula-sdk@<digest>` --
which docker refuses unless the content hashes to it -- then checks the
digest again and tags the image locally. With no digest pinned for the
tree's tag, or none the registry serves (a fork's), the build derives the
image itself from the upstream digest `ci/build-sdk-image.sh` pins:
slower, and trusting no registry at all.

Who can publish one (finding "Any branch or dependency build script with
packages:write can overwrite the SDK image that release builds pull by
mutable tag"):

- **`rpm.yml` cannot.** It runs a pull request's scripts and every crate's
  build script, so it holds `packages: read` and no more, signs in to the
  registry only for the pull and signs out before any build step, and
  checks out with `persist-credentials: false`, so no token is left on the
  runner for a build script to find.
- **`sdk-image.yml` can, from `main` only** (`if: github.ref ==
  'refs/heads/main'`): the one workflow with `packages: write`, signed in
  only after the derivation, for the push. Its summary prints the
  `<tag>  <digest>` line to commit to `ci/sdk-image.digests`.

A dispatch from another branch runs that branch's copy of `sdk-image.yml`
and could drop the check, and could push over main's tag -- which is why
the digest file, not the registry, is what builds trust.
`ci/packaging-lint.sh` holds all of this: no concurrency group on
`head_ref`, no `packages: write` outside `sdk-image.yml`, `sdk-image.yml`
refusing other branches, no `docker pull` in `rpm.yml`, none but by digest
in `scripts/build-rpm.sh`, and the release's ancestry check (below).

To move to a new image: change the SDK version, `ci/build-sdk-image.sh` or
the BuildRequires, merge, run `sdk-image.yml` on `main`, and commit the
line it prints. Until then every build derives the image, which costs
about as long as the build itself.

`scripts/build-rpm.sh package <image>` then runs mb2 inside it:

- **as the image's own build user**, never root -- `sdk-manage` refuses
  root -- so the tree is chowned to that user for the build and back after;
- **mounted inside that user's home**, as `$HOME/harbour-sukkula`: rpm runs
  under scratchbox2, which redirects absolute paths it does not recognise
  into the target rootfs, and with the tree anywhere else mb2 writes
  `.mb2/spec` outside and rpm looks for it inside; and mb2 finds the spec by
  the directory's name;
- `mb2 -t SailfishOS-5.2.0.15-aarch64 -X build-init`, `build-requires`,
  `build --no-check` -- `-X` because without it build-init asks git for a
  version, finds no tags, and stops before writing the spec.

The spec (`rpm/harbour-sukkula.spec`) links the engine and never builds
it: `%build` fails at once, with a pointer here, when the library is
missing. It runs qmake **out of tree**, in `build-sfos/` -- qmake writes a
`Makefile` where it runs, and the root one is the developer targets
(`ci/packaging-lint.sh` checks). `SUKKULA_RUST_LIB` hands qmake the
library's path; `--define "sukkula_rust_lib /elsewhere/libsukkula_ffi.so"`
overrides it. qmake links the binary against it and installs it in
`/usr/share/harbour-sukkula/lib`. `%install` runs qmake's install, then
strips the binary and the library (nothing else does;
`docs/HARBOUR.md`).

Locally, with Docker and sudo:

```sh
make rpm                     # = scripts/build-rpm.sh all
```

which uses the image already on this machine, or pulls the pinned one by
digest, or derives it; lifts the toolchain into `~/.cache/sukkula-sdk/`, cross-builds the engine, runs mb2, and judges the
result exactly as `rpm.yml` does.

## Offline builds

Nothing in the SDK needs crates: the engine is built on the host before
mb2 runs. To build the engine with no network -- an air-gapped machine, or
a release build that must not depend on crates.io being up:

```sh
cargo vendor --locked --versioned-dirs vendor > .cargo/config.toml
CARGO_NET_OFFLINE=true scripts/cross-build-rust.sh --sdk <sysroot>
```

`cargo vendor` copies every crate of the locked graph, the LocalSend git
dependency included, into `vendor/`, and prints the source replacement
that `.cargo/config.toml` then holds; both are gitignored. `--locked`,
which every build here passes, keeps the vendored set and `Cargo.lock` in
step.

## CI

`ci.yml` is the gate; every job is required. Actions are pinned by commit
(`.github/dependabot.yml` moves the pins), the token is `contents: read`
everywhere but the two places that write (the SDK image, from `main` only,
and the release), a pull request's runs are grouped by its number -- never
by `head_ref`, a fork's bare branch name, which let two forks' `main`
cancel each other's runs -- and
packages come through `ci/apt-install.sh`, which drops the runner image's
third-party apt lists before `apt-get update` so one of them serving a bad
index cannot fail every job (`ci/apt-install-selftest.sh` proves it keeps
Ubuntu's archive).

`rpm.yml` runs on `v*` and `build-*` tags, from the Actions tab, and on
pull requests that touch the package (`docs/HARBOUR.md` says which paths
and why). Each build is stamped `Release: 1.<run number>` -- digits and
periods only, as Harbour requires -- so each one installs over the last;
a release keeps the spec's `Release: 1`, has to match the spec's
`Version:`, is cut from `main` (a dispatch has to run on `main`, and a
`v*` tag has to name a commit `main` contains: `git merge-base
--is-ancestor`, on the job's full-history checkout), and is published by
a job of its own after the package passed the validator.

## Static analysis

SonarQube Cloud reads the tree on every push to `main` and every pull
request from this repository (`.github/workflows/build.yml`, configured by
`sonar-project.properties`), except Dependabot's: those run without the
repository's secrets, so their changes are analysed when they reach
`main`. It is a **report, not a gate**: `ci.yml`
decides what is allowed in, and nothing Sonar says can turn a red build
green or a green build red. That is why it is a workflow of its own.

The scanner **imports** coverage; it does not measure it. `make
sonar-reports` writes `target/sonar/lcov.info` with `cargo llvm-cov` over
the workspace's tests, and the workflow runs it before the scan. It needs
`cargo-llvm-cov`, so it is not part of `make check`:

```sh
rustup component add llvm-tools-preview
cargo install --locked cargo-llvm-cov
make sonar-reports
```

`sonar-project.properties` says what is analysed and why: test code apart
from the application, upstream's `third_party/` and the generated
`translations/` left out, Sonar's own Clippy pass off (the gate's is the
one that counts), and `src/` and `qml/` out of the coverage arithmetic,
since nothing measures them. C and C++ are analysed without a build
wrapper (SonarQube Cloud deduces the compiler options itself).

The scanner uploads a report and exits; the server processes it
afterwards, so the run that produced an analysis finishes knowing nothing
about its result. `scripts/sonar-report.sh` asks the server from the
runner that just fed it and prints the quality gate, the measures and the
open issues into the job log and the step summary, where they can be read
without a sonarcloud.io login. A section the server refuses is named in
the report, with both refusals (token and anonymous) in the job log, and
fails the step. The step is `continue-on-error`: a Sonar outage costs a
warning, not a build.

## Cutting a release

1. Set `Version:` in `rpm/harbour-sukkula.spec` and the workspace version
   in `Cargo.toml`, refresh `Cargo.lock`, merge.
2. Dispatch `rpm.yml` on `main` with the version in `release`, or push the
   tag `v<version>` on the merge commit.
3. The package on the release page is the one to submit to Harbour.

`rpm.yml` refuses a `v*` tag on a commit `main` does not contain, before
anything is built (finding "A 'v*' tag on any commit publishes a GitHub
release"). That guards against a mistake, not an attack: a tag runs the
workflow file of the commit it names, which whoever pushed the tag
controls. The repository's settings close that: a **tag ruleset** on `v*`
that lets only the maintainers create, move or delete such a tag, and a
branch ruleset on `main` requiring `ci.yml`'s jobs. Neither lives in the
tree, so neither is checked here; set both when the repository is created.

## Interop with the reference clients

Spec §7's adapter layer asks for Sukkula against each protocol's reference
client on the same host, as `cargo test`:

- **LocalSend**: `crates/sukkula-engine/tests/localsend_loopback.rs`, in
  the `test` job -- upstream's own core crate is the other side.
- **Magic Wormhole**: `tests/wormhole_interop.rs` runs the Python client
  (magic-wormhole 0.24.0) both ways -- a text, a file and a folder to
  Sukkula, a file and a text from it -- against the tests' own mailbox and
  relay on loopback. Its tests are `#[ignore]` on a machine without the
  client; the `wormhole-interop` job installs it into a venv with `pip
  install --require-hashes --only-binary=:all:` from
  `ci/wormhole-interop-requirements.txt`, which pins it and everything it
  pulls in by version and hash, on a pinned Python, runs them with
  `--include-ignored`, and fails if none ran. Locally: `make
  wormhole-interop` (`PYTHON=` names another 3.12). To move to a newer
  client, run the tests against it by hand, then regenerate the file with
  the command in its header.
- **Quick Share: no reference-client test.** `tests/quickshare.rs` is
  Sukkula to Sukkula and a hand-built hostile sender. rquickshare is a
  desktop GUI over the same `rqs_lib` Sukkula vendors, so a test against
  it would mostly be the library against itself; what would differ is the
  patches (`third_party/rqs_lib.patches/`), and an unpatched upstream
  build as the peer is not in the tree. Interop with real Android Quick
  Share is manual: M-30 to M-34 in `docs/MANUAL-TESTS.md`, on hardware.
  This is a known gap against spec §7.

## Dependencies

Spec §6 is the policy: each protocol from one maintained library, one line
of reason per dependency, and a design note for anything that brings a
second async runtime, TLS stack or D-Bus stack. Three things hold it:

- **`deny.toml`**: licences against an allow-list (copyleft granted per
  crate: EUPL-1.2 to magic-wormhole, GPL-3.0 to rqs_lib); advisories, with
  RUSTSEC-2023-0071 (`rsa`) excepted and the reason written; duplicate
  versions denied, the ones the graph has today listed with their cause;
  OpenSSL, native-tls, aws-lc, zbus and platform TLS bindings banned, and
  `vendored` banned on dbus and libdbus-sys; crates.io and the pinned
  LocalSend repository as the only sources.
- **`ci/check-deps.sh`**: the same budget over `Cargo.lock` and over
  sukkula-ffi's real aarch64 graph -- one TLS stack, crypto provider,
  D-Bus stack; no vendored libdbus; no async runtime beyond tokio and
  magic-wormhole's smol pieces; no crate but the engine using `dbus`, and
  none but `dbus` using libdbus-sys (libdbus runs `autolaunch:` and
  `unixexec:` addresses, and only the engine's connection code is held to
  `clippy.toml`'s constructor bans); Bluetooth stacks and crates that spawn
  processes or open URLs and files (process runners, pseudo-terminals,
  `nix`, `open`, `opener`, `webbrowser` and the like) only with an entry
  in `ci/deps-allow.conf`, whose `dev` scope is proved absent from what
  ships.
- **`ci/check-lockfile.sh`**: every git dependency pinned by `rev` to a
  full commit that the lockfile resolves to; the lockfile current; no path
  dependency outside the tree.
