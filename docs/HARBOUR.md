# Harbour

Sukkula is distributed through [Jolla's Harbour store](https://harbour.jolla.com)
and nowhere else: no OpenRepos, no Chum, and nothing in the tree that exists
only for them (spec §1, §8). It targets the Jolla Phone 2026 and nothing
else -- Sailfish OS 5.2 and later, aarch64 (spec §2). There is no armv7hl or
i486 build, no branch in the packaging for one, and no compatibility code for
older releases.

Harbour's rules are not advice. A validator failure is a guaranteed
rejection, and several rules constrain things -- the package name, the
install paths, the sandbox permissions, the linked libraries -- that are
expensive to change once code is built around them. Spec §2 puts anything
the validator rejects out of scope rather than worked around. So the rules
are CI gates, on every pull request.

## The two checks

| | `ci/harbour-check.sh` | `ci/harbour-validate-rpm.sh` |
|---|---|---|
| Runs | every pull request (`ci.yml`, job `harbour`) | every RPM build (`rpm.yml`): tags, dispatch, and pull requests that touch the package |
| Reads | the source tree | the built package |
| Needs | rpm, file | the Sailfish SDK, ten-odd minutes of runner time |
| Is | Sukkula's reimplementation, plus Sukkula's own stricter policy | Jolla's own `rpmvalidation.sh`, what `sfdk check -s harbour` runs |
| Authority | no | **yes** |

The second one decides. The first exists because the second cannot run on
every push, and because a rule broken in a pull request is cheaper to fix
than one discovered at intake.

Both are **mandatory**. Neither has a warn-only mode: the source check fails
on any finding, `HARBOUR_CHECK_STRICT=1` (set in CI) makes a missing tool a
failure rather than a skip, and a missing input -- no `.desktop` file, no
icons, no QML, no `src/main.cpp` -- is a failure too, because a tree without
them is a tree Harbour would reject. The RPM check fails on every `ERROR`
*and every `WARNING`* the validator prints. Harbour itself rejects on errors
only, but each warning it can raise about this package (an unstripped
binary, a missing icon size, the Compatibility permission, a deprecated
library, an RPATH) is either a defect or QA scrutiny at intake, and Sukkula
has no reason to carry one.

They are kept honest against each other. `ci/harbour/` holds the
validator's own allow-lists, copied verbatim at one upstream commit by
`scripts/update-harbour-rules.sh` (`ci/harbour/UPSTREAM` names it). The RPM
check fetches the validator from raw.githubusercontent.com *at that same
commit*, refuses its two unvendored files unless they hash to what
`UPSTREAM` records, and refuses to run if any vendored `.conf` differs from
the fetched one -- so the rules the source check reads and the code the RPM
check runs are the same Harbour, and a hand edit to a vendored rule fails.

And each is tested: `ci/harbour-check-selftest.sh` breaks every rule below
in a throwaway copy of a fixture tree (`ci/fixtures/harbour/`) and asserts
the check names it by ID -- a hundred-odd cases, including the changes the
check must *accept* (a doc comment mentioning a home directory, bad imports
in `tests/` and `qml-stubs/`, which never ship) -- and feeds the RPM
wrapper saved validator logs to prove how it judges them. A gate that only
ever prints "ok" is indistinguishable from one that has stopped looking.

## Why rpm.yml runs on pull requests

piirit and vuo build packages only on demand. Sukkula also builds one for
every pull request that touches what the package is built from: the Rust
crates and `third_party/` (the engine), `src/` (the C++ shell, its link
flags in `src/hardening.pri` and its export list `src/dynamic.list`),
`qml/`, icons, translations, the spec, the `.pro`, the `.desktop` file,
`Cargo.lock`, every `Cargo.toml` and `build.rs`, `rust-toolchain.toml`,
and the Harbour rules and scripts that build and judge the RPM.

Two reasons. Three Harbour rules can only be answered by the built
package: the `Requires` rpm generates from the binary's symbol versions,
the shared libraries the final link keeps, and whether `main()` survives
the strip. A dependency bump can change the first two without touching a
line of packaging -- a crate whose build script starts linking a system
library that is not on the allowed list (libudev, libsystemd); one
compiled against newer glibc headers needs a symbol version the phone
lacks -- and the per-PR `cross` job, built with Ubuntu's toolchain, cannot
see the phone's glibc. And the validator runs `strings(1)` over every file
in the package, so a string in the code is the package's: a home
directory in a Rust `#[error("...")]` attribute or a C++ `#define` is an
`ERROR 'Hardcoded path'`, whatever the source check makes of it. The
paths once left out `crates/`, `src/` and `qml/`, so such a change merged
with neither check looking at it (finding "Harbour source gate skips
every '#' line"); P.6 now fails a tree whose `rpm.yml` leaves any of them
out.

## What the source check covers

IDs are the check's own. The numeric ones are the numbering Sukkula shares
with piirit and vuo, each a rule of `rpmvalidation.sh`; `P.n` are Sukkula's
policy, stricter than Harbour.

| ID | Rule |
|---|---|
| 1.1.1 | package name `^harbour-[-a-z0-9_.]+$` |
| 1.1.2 | the RPM file name, with the Release rpm.yml stamps, is at most 100 characters |
| 1.1.3 / 1.1.4 | Version digits and periods; Release digits, underscores and periods -- *including every Release `rpm.yml` stamps*, and the stamp must exist |
| 1.1.5 | `ExclusiveArch: aarch64`, and nothing else |
| 1.2.1 | every `%files` path is `/usr/bin/<NAME>`, `/usr/share/<NAME>/**`, its `.desktop` file or its icons; no `%doc`/`%license` (rpm puts those elsewhere) |
| 1.2.2 / 1.2.3 | the `.desktop` file and the binary are packaged; the qmake `TARGET` is the package name |
| 1.2.4 / 1.2.6 | no debug directories, nothing under `/home` |
| 1.2.5 | no `.git`, editor backups, swap files or `.DS_Store` in the trees qmake installs wholesale (`qml/`, `translations/`, `icons/`) |
| 1.2.7 / 1.2.8 | no group- or world-writable, setuid, setgid or sticky modes; everything owned by root |
| 1.2.9 | no executable file in those trees: qmake's directory install keeps each file's mode, and only the binary may be executable |
| 1.3.1 -- 1.3.7 | `Name=`; `Exec=<NAME>` exactly (a C++ app, never `sailfish-qml`); `Icon=<NAME>`; `Type=Application`; `X-Nemo-Application-Type=silica-qt5`; `[X-Sailjail]`, never `[Sailjail]`, never empty |
| 1.4.1 -- 1.4.7 | `OrganizationName` and `ApplicationName` against their patterns and the reserved list; every permission on the whitelist, and not `Compatibility`; `ExecDBus` agreeing with `Exec`; only the allowed keys |
| 1.5.1 -- 1.5.4 | all four icon sizes present, real PNGs of exactly their directory's size, and installed |
| 1.6.1 | every library the `.pro` links (`LIBS`, `QT`, `PKGCONFIG`) is on the allowed list -- no `-lutil`, no `widgets` |
| 1.6.4 -- 1.6.6 | every QML import against the allowed, deprecated, dropped and blocked lists; no absolute imports; relative ones resolve inside the installed tree |
| 1.6.7 | no ELF file in the shipped trees |
| 1.6.8 | no `tests/` or stub directory inside `qml/`, where it would ship |
| 1.7.3 | `Q_DECL_EXPORT int main` in `src/main.cpp`, and `CONFIG += sailfishapp`, whose `-rdynamic` puts it in `.dynsym` |
| 1.8.1 -- 1.8.7 | no `Vendor:`; no `Provides:`, `Obsoletes:`, `Conflicts:`, `Recommends:`, `Suggests:`, `Supplements:`, `Enhances:`; every `Requires:` unversioned and allowed; no scriptlets or triggers of any kind; XmlListModel required if imported; no `libsailfishapp-launcher` |
| 1.8.8 | the spec filters `libdbus-1.so.3(LIBDBUS_1_3)(64bit)` from the generated Requires, and nothing else (below) |
| 2.1 | no `/home/nemo` or `/home/defaultuser` in anything compiled or shipped: C++, QML, JavaScript, the Rust crates and `third_party/` with only real comments (`//`, `/* */`) dropped -- a `#[...]` attribute, a `#define` and a line starting with `*` are code -- and every other file under `src/`, `qml/` and a crate's `src/` read whole, since `include_str!` or a Qt resource puts it in the binary |
| 2.5 | the app's data path follows `OrganizationName/ApplicationName` |
| 2.6 | nothing writes to a path the package installs |
| 2.7 | every platform QML module imported has its package required (`Nemo.Notifications`, `Nemo.KeepAlive`) |
| P.1 | no other device architecture anywhere in the spec, the workflows or the build scripts |
| P.2 | the sandbox permissions are exactly `Internet;Bluetooth;Downloads` |
| P.3 | every SDK version the packaging can build against is 5.2 or later |
| P.4 | `OrganizationName=sukkula`, `ApplicationName=sukkula` |
| P.5 | platform QML modules are exactly spec §2's: Sailfish.Silica, Sailfish.Share, Sailfish.Pickers, Nemo.KeepAlive, Nemo.Notifications |
| P.6 | `rpm.yml` runs Jolla's validator on every pull request that changes the package: its `pull_request` trigger has no `paths:` filter, or one (a block list, no `paths-ignore`) that names the crates, `third_party/`, `src/`, `qml/`, icons, translations, the spec, the manifests and the lockfile |

## What only the built package shows

`rpm.yml` runs these on every RPM, and fails the job on any of them:

- **`ci/check-elf.sh`** on `/usr/bin/harbour-sukkula` out of the package:
  stripped; `main()` a defined dynamic symbol, and nothing else of ours
  exported (`--only-main`: every other defined dynamic symbol has to be one
  the C runtime or the linker script puts in every executable -- never a
  Bridge method, a `sukkula_*` entry point or a Rust symbol);
  `__libc_start_main@GLIBC_2.34` linked; no glibc symbol version newer than
  the target sysroot's own; every `NEEDED` library on the allowed list;
  RELRO, BIND_NOW, PIE, no text relocations, a non-executable stack; the
  one RPATH `/usr/share/harbour-sukkula/lib` that the SDK's `sailfishapp`
  feature sets, as DT_RPATH -- the validator reads only that, and fails a
  package that ships a library without it -- and no RUNPATH; and a TLS
  segment that is exactly the 4096 zero bytes at `tp+16` the phone's
  graphics stack takes (`src/tls_reserve.c`).
- **`ci/check-elf.sh --library`** on the engine's
  `/usr/share/harbour-sukkula/lib/libsukkula_ffi.so`: stripped; exactly the
  four `sukkula_*` functions exported (`crates/sukkula-ffi/exports.map`),
  and no Rust symbol; `SONAME` its file name; the same glibc ceiling,
  allowed-library rule and hardening; and thread-locals reached through TLS
  descriptors, never the static models, so they still work when the
  booster `dlopen()`s the binary (`docs/FFI.md`, Linking).

  The same two checks run on every pull request against what
  `scripts/cross-build-rust.sh` builds: the library, and a probe linked
  against it with the shell's flags.
- **`ci/harbour-validate-rpm.sh`**: Jolla's validator itself.

## Sailjail permissions, and why each

`Permissions=Internet;Bluetooth;Downloads` and nothing else (spec §2). P.2
fails a tree that asks for less or for more: each one missing is a feature
that silently does not work in the sandbox, and each one extra is reach
nobody reviewed.

- **Internet** -- LocalSend and Quick Share over the LAN (multicast
  discovery, mDNS, the HTTPS and TCP servers peers connect to), and Magic
  Wormhole's mailbox and relay servers.
- **Bluetooth** -- BlueZ on the system bus (`org.bluez`) and obexd on the
  session bus (`org.bluez.obex`): OBEX Object Push for sending (F-BT1), and
  the BLE advertisement that makes Android phones reveal their Quick Share
  service (F-QS2). The engine reaches both through the system
  `libdbus-1.so.3`; QtBluetooth is not allowed.
- **Downloads** -- received files land in `~/Downloads/Sukkula/`, staged in
  its hidden `.partial/` (S3). Staging there, not in the app's data
  directory, is deliberate: Sailjail's bind mounts make a link or rename
  from the data directory into `~/Downloads` fail with `EXDEV`.

**To verify on hardware** (milestone 1): whether a file handed to Sukkula
through the Share menu, or chosen with the Silica file picker, from outside
`~/Downloads` -- a photo in `~/Pictures` -- is readable by the sandboxed
app. Silica's pickers run inside the app's process, so the sandbox decides
what they can open; piirit found it needed `UserDirs` for exactly that. If
the answer is no, the fix is a spec change (a permission added to §2 and to
`POLICY_PERMISSIONS` in `ci/harbour-check.sh` in the same commit), never a
workaround.

## The SDK version is a Harbour rule

Harbour requires the binary to link `__libc_start_main@GLIBC_2.34`
(`rpmvalidation.conf`), and the version is the point: 2.34 is where glibc
merged libpthread and libdl into libc and re-versioned the symbol. A binary
built against an older glibc references `@GLIBC_2.17` on aarch64 and is
rejected -- piirit's first validator run, against a 4.6 SDK, failed on
exactly this. Only a 5.x SDK provides it, and Sukkula builds against
**5.2.0.15**, the Jolla Phone 2026's baseline: a binary from a newer SDK can
need symbols a phone on 5.2 lacks. `ci/build-sdk-image.sh` pins that SDK by
digest and knows no other; P.3 fails a workflow that names an older one.

## Exporting `main()`, and stripping

The `silica-qt5` booster in mapplauncherd `dlopen()`s the binary and looks
`main` up dynamically, so the validator rejects a Silica app whose binary
does not export it. The C++ entry point is `Q_DECL_EXPORT int main(...)`,
and `CONFIG += sailfishapp` links with `-rdynamic` under
`-fvisibility=hidden`, which puts `main` -- and only the exported symbols --
into `.dynsym`. The engine is a library of its own (`docs/FFI.md`,
Linking), whose exports its link restricts to the C ABI. The binary is
linked with `--dynamic-list=src/dynamic.list` (main alone) and
`--exclude-libs,ALL` all the same (`src/hardening.pri`): `-rdynamic` alone
would export every global symbol of anything static linked into it.
`ci/check-elf.sh --only-main` holds the packaged binary to that: before
it, only a host test (`tests/run-cpp-tests.sh`), linked with a stand-in of
the SDK's `sailfishapp.prf`, checked that nothing else was exported (finding
"check-elf.sh does not check the 'only main exported' rule").

Nothing strips the binary by default: piirit found that the SDK's rpmbuild
does not (every package it built carried "file is not stripped!"), and
qmake's install is told not to. The spec's install section runs
`strip --strip-all` itself, which drops `.symtab` and keeps `.dynsym`, so
`main` survives; `ci/check-elf.sh --stripped --main-export --only-main`
proves all three on the packaged binary.

## libdbus-1's versioned Requires

The engine links the system `libdbus-1.so.3` (Harbour-allowed), whose whole
API is versioned `LIBDBUS_1_3`. rpm therefore derives two Requires from the
binary: `libdbus-1.so.3()(64bit)`, which `allowed_libraries.conf` covers,
and `libdbus-1.so.3(LIBDBUS_1_3)(64bit)`, which neither allow-list carries --
the validator rejects it as "Cannot require shared library". The spec's
`__requires_exclude` drops exactly that one string, which is the mechanism
the Harbour FAQ names for generated dependencies. The unversioned
requirement stays, and it is the real one. Check 1.8.8 proves the filter
matches it and matches nothing else (not the unversioned form, not glibc,
not Qt).

## Waivers

`ci/harbour/waivers.conf` is where a rule this package knowingly breaks
would be recorded, one line each with its reason:

```
<checker>  <id>  <subject glob>  <message glob>  # why
```

The checker says whose finding it is. `source` is `ci/harbour-check.sh`,
and the id is one of its check IDs above; `rpm` is Jolla's validator
through `ci/harbour-validate-rpm.sh`, and the id is its severity, `ERROR`
or `WARNING` (the validator names no check). Each check reads only its own
lines and waives a finding only when the id, the subject glob *and* the
message glob all match, so a waiver excuses one known finding and never
every finding about a path; a message glob of nothing but `*` and `?` is
refused, and so is a line with no reason. Each check fails on a waiver of
its own that matched nothing, so an entry cannot outlive what it excuses,
and both fail on a malformed line of either kind. `ci/harbour-waivers.sh`
is the one parser.

That is a fix (finding "Waiver scope differs between the two Harbour
checks"): the file used to have no checker column, the RPM check ignored
the id, and the source check read the same line by id and subject alone.
A waiver written for the source check, with the message `*` as its
selftest wrote them, then waived every validator finding about its path --
"Binary must export main()" and "Hardcoded path" included -- and a waiver
for a validator-only finding could not be written at all, because the
source check called it stale.

**It is empty, and is meant to stay empty.** Sukkula has no known Harbour
blocker; the spec puts anything the validator rejects out of scope. A
waiver is not a way to ship around a finding -- it is a record of a blocker
that stops submission, kept so the gate stays honest while the blocker is
worked on. Renaming a file to pass a rule (an executable called `.so`, say)
is what Jolla calls circumvention, and it gets apps removed after approval.

## Keeping the rules current

```sh
scripts/update-harbour-rules.sh            # upstream HEAD
scripts/update-harbour-rules.sh <commit>   # a specific commit
```

It rewrites `ci/harbour/*.conf` and `ci/harbour/UPSTREAM` (commit, date,
the two hashes). Review the diff, run `ci/harbour-check.sh` and its
selftest, and follow any rule whose logic moved in `rpmvalidation.sh` itself.
