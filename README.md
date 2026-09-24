# Sukkula

Send and receive files and text over LocalSend, Quick Share, Magic Wormhole
and Bluetooth, from one Sailfish OS app.

> 🤖 **This project was developed using AI.** If that provenance troubles
> you, feel free to use something else. That being said, Sukkula writes
> adapters, not protocols: every protocol comes from its own maintained
> upstream library.
>
> 📱 **Modern Sailfish OS only:** Sukkula targets the Jolla Phone 2026
> (Sailfish OS 5.2+, aarch64) and nothing else. No effort is made to
> accommodate older releases, other devices or other architectures.
> [Buy a Jolla Phone 2026](https://commerce.jolla.com/) and support
> European-made alternatives. 👊🇪🇺🔥
>
> 🏪 **Harbour only:** Sukkula is distributed through Jolla's Harbour store
> and nowhere else. No OpenRepos, no Chum.

## Overview

*Sukkula* is Finnish for "shuttle" -- the loom's and the space kind. It
carries things back and forth, which is the whole app.

| Protocol | Send | Receive | Peer needs |
| --- | --- | --- | --- |
| LocalSend v2 | yes | yes | the LocalSend app, same LAN |
| Quick Share | yes | yes | stock Android, same LAN |
| Magic Wormhole v1 | yes | yes | any wormhole client, internet |
| Bluetooth OBEX Object Push | yes | no (the system handles it) | Bluetooth |

Every incoming offer is shown before a byte is written -- sender, protocol,
file names, sizes -- and there is no auto-accept. Received files go to
`~/Downloads/Sukkula/`. Nothing runs while the app is closed. Every peer is
treated as hostile: names are sanitised, sizes range-checked, every read
bounded and timed out. The specification, with its security rules, is
[`docs/SPEC.md`](docs/SPEC.md).

All logic is Rust (`crates/`): `sukkula-core` is the trust boundary,
`sukkula-engine` the protocol adapters, `sukkula-ffi` the C ABI the
Qt/Silica shell links.

## Limitations

- aarch64 and Sailfish OS 5.2 or later, and nothing else.
- No AirDrop, no Wi-Fi Direct, no LocalSend web share.
- No receiving while the app is closed.
- No OpenRepos/Chum.

## Documentation

- [`docs/SPEC.md`](docs/SPEC.md) -- scope, requirements and security rules
- [`docs/BUILDING.md`](docs/BUILDING.md) -- toolchains, tests, the
  cross-build and how a device RPM is made
- [`docs/HARBOUR.md`](docs/HARBOUR.md) -- Jolla's store rules and the two
  CI gates that hold them

## Building

Everything but the device package needs no phone and no Sailfish SDK:

```sh
make check      # exactly what CI runs on a pull request, minus the network
make deny       # licences and advisories (network)
make cross      # the engine for aarch64, with Ubuntu's cross GCC
```

The device RPM is built in the Sailfish SDK container, with the engine
cross-compiled by the pinned Rust against the SDK's own aarch64 GCC and
Sailfish OS 5.2 sysroot, then judged by Jolla's own Harbour validator:

```sh
make rpm        # needs Docker; see docs/BUILDING.md
```

## Licence and credits

Sukkula is GPL-3.0-or-later; see [`LICENSE`](LICENSE). The package links:

* [LocalSend](https://github.com/localsend/localsend)'s own Rust core, for
  LocalSend v2 -- Apache-2.0.
* [open-quickshare](https://github.com/ignotusbucius/open-quickshare)'s
  `rqs_lib`, for Quick Share -- GPL-3.0, vendored in `third_party/` with
  Sukkula's security patches kept separate.
* [magic-wormhole.rs](https://github.com/magic-wormhole/magic-wormhole.rs),
  for Magic Wormhole -- EUPL-1.2, conveyed under the GPL by its
  compatibility appendix.

Thanks to all three projects for the protocols this app only carries.
