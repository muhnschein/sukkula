# Host tests of the Qt/QML shell

Everything here runs offline on an x86_64 Linux host with a desktop Qt 5.15,
headless (`QT_QPA_PLATFORM=offscreen`, software rendering). The phone runs
Qt 5.6; the app's QML stays within it (checked statically), and the stubs
that stand in for Sailfish's modules are host-only.

## Packages (Ubuntu 24.04)

```sh
sudo apt-get install -y qtbase5-dev qtdeclarative5-dev qtdeclarative5-dev-tools \
    qml-module-qtquick2 qttools5-dev-tools libdbus-1-dev binutils python3 g++ make
```

`qttools5-dev-tools` provides `lupdate` and `lrelease`; without it
`/usr/bin/lupdate` is only a qtchooser link to nothing.

## The two entry points

```sh
tests/run-qml-tests.sh                     # QML: lint, static rules, runner, self-test
tests/run-cpp-tests.sh                     # C++: bridge, host app, ELF checks, install layout
SUKKULA_ENGINE=rust tests/run-cpp-tests.sh # the same, also against the real engine
```

Both exit non-zero on any failure and print one `ok`/`FAIL` line per
stage. They build into `build/host-tests/` (git-ignored); set `BUILD_DIR`
to put that elsewhere, `QMAKE`/`LRELEASE` to pick other tools. For
`SUKKULA_ENGINE=rust`, build the library first:

```sh
CARGO_BUILD_JOBS=2 cargo build -p sukkula-ffi   # target/debug/libsukkula_ffi.a
```

(`SUKKULA_RUST_LIB` points at another one.) `tests/run-one-qml-test.sh
tst_consent.qml` runs single QML tests without the other stages.

## What `run-qml-tests.sh` checks

1. **qmllint** on every `.qml` file of `qml/`, `qml-stubs/` and `tests/qml/`.
2. **`tests/qml/static_checks.py`**: every `Label`/`Text`/`TextEdit` in
   `qml/` says `textFormat: Text.PlainText`, and nothing asks for rich,
   styled or auto text (S2); no `Qt.openUrlExternally`, `linkActivated` or
   the like (S8); only the QML imports spec §2 lists, at Harbour's
   versions, each platform module in the one file that needs it, relative
   imports inside `qml/`, and every `Qt.resolvedUrl` naming a file; no
   JavaScript or QML newer than Qt 5.6 (`let`, `const`, arrow functions,
   template strings, `enum`, `function onX` handlers, `Qt.callLater`,
   required properties); the desktop file's share methods, the
   `ShareProvider`s and their descriptions (in every language) agree
   (F-C6); `[X-Sailjail]` is exactly spec §2's permissions with the
   names `src/main.cpp` uses (§2); and every catalogue holds exactly
   the strings of the sources, all translated, with the same placeholders
   (run `scripts/update-translations.sh` after changing a string).
3. **The runner** (`tests/qml/runner`, built with qmake) loads each
   `tests/qml/tst_*.qml` with `qml-stubs/` and a fake bridge and runs its
   steps. On top of the tests' own assertions it fails a test on **any Qt
   warning** (a binding that did not bind, an unknown property, a type
   error), and between every two steps it walks the whole object tree:
   every text item created by a file under `qml/` must be
   `Text.PlainText`, and every text item anywhere that shows a peer string
   -- the tests spell them with `EVIL` -- must be too. The stubs draw the
   strings given to Silica components (`PageHeader`, `TextSwitch`,
   `Button`, `DialogHeader`, ...) with plain `AutoText` items, so a peer
   name handed to one of those instead of to the app's own label fails.
   The English catalogue is loaded, so plural forms read as on the phone.

   | Test | Covers |
   | --- | --- |
   | `tst_engine.qml` | Engine.qml against api.rs: command envelopes and ids, callbacks, bridge return codes, every event type, caps and bounds, garbage events, a peer's address and certificate of the right shape or nothing, the words for what files are |
   | `tst_consent.qml` | the consent dialog: sanitised values verbatim as plain text, what and how in Sukkula's words, PIN, one file big or several with "and N more" and the total, countdown; Accept, Decline, leaving, timeout and engine-closed paths (F-C2, F-C3, F-QS3, S5) |
   | `tst_main.qml` | main page, History page, text page, cover: Send \| Receive tabs as the engine's state, laid out as Silica's TabBar (the words centred together, a hairline under the one on screen), the list cut at the tabs' edge and taken to its top by a tap on its tab, tapped or swiped, a swipe back mid-switch switched back, a refused switch taking the tab back, discovery on the Send tab only, receiving stopped in the background but not while an offer waits, portrait only, what receiving is doing, transfers, cancel, texts with Copy, About at the foot of Settings; the cover's halves and actions, its offer and its ring, naming nobody (F-C1, F-C4, F-C5, spec v0.7) |
   | `tst_send.qml` | the Send tab with every protocol: the question and a picker per tile, who is nearby at the foot; files summed up with their size, more added, all cleared behind a remorse; the devices by name as plain text, one row for a device found two ways with a menu to choose, About this device with its pinned certificate; a send's row with progress, cancel and end, the files kept after it, refusals; sending with a code, Magic Wormhole or croc by the number of files, its QR code, Copy and Share, "Their app" giving the old code up, a code given up when its page is left unused; protocols switched off in Settings (F-C1, F-C6, F-MW1, F-CR1, F-BT1) |
   | `tst_receive.qml` | the Receive tab: ready with the name others see, a transfer coming in as plain text with its progress, cancel and the moment it stays, what came today with files that came together as one row and their own page, one file by its name, a text, a failure not among them; the radar taking the room the rest leaves; scanning a QR code and typing a code in, each its own page, gone with both code protocols off; a way that could not start said in red, leading to Settings (F-C1, F-C5, F-MW2) |
   | `tst_scan.qml` | receiving with a code (spec v0.6): the viewfinder handed over a frame at a time and only while the page is on top, in front and with a camera, asked for a big enough picture and to focus; nothing read and non-engine JSON ignored, a QR code of anything else said to be no code and never shown, the code received over its own protocol with its QR code's mailbox, a protocol switched off refusing it; no camera leading to the typing page in the scan page's place; on that page, the keyboard up, the typed code sent as typed for the engine to tell its protocol (`receive_code`), a failure said with the code kept, the clipboard offered only when it holds a code, typing without a scanner (F-C1, F-MW2, F-CR2) |
   | `tst_settings.qml` | settings validation, the switches named by who they reach, how each way of receiving nearby is doing under its switch and why one could not start in red, options greyed under a switch that is off, one's own servers folded away until set or wrong, each empty field naming its server, saving on leaving and not when covered, the Magic Wormhole switch, About (F-C1, F-C7, F-LS4, F-QS2, F-QS4, F-MW4, S9) |
   | `tst_app.qml` | the whole window: start-up, discovery paused after a while in the background, consent queueing over any page and around transitions, the Share menu's files (and not its texts), the cover's actions opening their tab, KeepAlive, notifications (§2, F-C6) |
   | `tst_stack.qml` | the whole window while pages cover each other: a consent dialog over Settings saves nothing, a share over another page keeps discovery and its peers, the Receive tab gives them back, send and code replies never pop a consent dialog, a closed offer's dialog never stays under the next one and is not answered (F-C2, F-C3, F-C6, F-LS1, F-QS1, S5) |
4. **`tests/qml/selftest.py`** first requires every check to pass on an
   untouched copy of the tree, then plants 71 faults one at a time -- a
   label without `PlainText`, the sender in a Silica header, a misspelt
   `Text.Plaintext`, rich text, a clickable link, an Accept that does not
   accept, a countdown that never declines, KeepAlive held forever, a peer
   name in a notification, `let`, a disallowed import, a share method
   nothing answers, an extra Sailjail permission, an unbounded queue,
   Settings saved whenever a page covers them, a reply that pops whatever
   is on top, a closed offer answered, a Magic Wormhole that cannot be
   switched off, devices listed before anything is chosen, receiving kept
   on in the background, a code left running when its page is left, a
   peer's words on the cover, ... -- and requires the check named for
   each to fail.

## What `run-cpp-tests.sh` checks

1. **`tests/cpp/bridge_test`** (QtTest) under ASan and UBSan, against the
   stub engine `tests/cpp/stub/sukkula_stub.c` (which keeps sukkula.h's
   threading rules) and, with `SUKKULA_ENGINE=rust`, against the real
   library: first events on the GUI thread and in order, replies by id,
   the StartConfig, NUL and 64 KiB refusals, `SUKKULA_ERR_BUSY`, a failed
   start's `fatal`, over-long events dropped, nothing delivered after
   `stop()`, and destroying the bridge in the middle of a 5000-event burst.
   Beside it **`tests/cpp/scanner_test`**, the same way: `src/scanner.cpp`
   making frames grey and at most 1024 pixels a side, one frame at a time,
   the answer on the GUI thread, an item in an offscreen window grabbed
   and read (the real engine decodes the QR code drawn in it), and a
   scanner destroyed with a frame in hand. LeakSanitizer runs with
   `tests/cpp/scanner_test/lsan.supp`, which names what Qt itself keeps
   until exit once a window has been shown, and nothing on a path of the
   scanner's.
2. **The host app** (`tests/cpp/host_app`): `src/main.cpp` and
   `src/bridge.cpp` with the phone's hardening flags (`src/hardening.pri`)
   and a libsailfishapp stand-in (`tests/cpp/sailfishapp`), started
   offscreen on `qml/` in Finnish: no Qt warning, the Finnish catalogue
   loads through main.cpp's translator, the engine's settings reach the
   main page, and (real engine) the data and download directories appear
   where Sailjail grants them.
3. **The binary**: `main` is the only dynamic export, BIND_NOW, GNU_RELRO,
   PIE, no `.symtab`, the one RPATH as DT_RPATH, and every NEEDED library on
   Harbour's list; for the `harbour-sukkula.pro` build also a TLS segment
   that is exactly `src/tls_reserve.c`'s 4096 zero bytes.
4. **`harbour-sukkula.pro` itself**, built out of tree with a stand-in of
   the SDK's sailfishapp feature (`tests/cpp/sailfishapp/features`, which
   passes `-rdynamic` and sets the RPATH as the real one does) against an
   engine library, and installed with `INSTALL_ROOT`: exactly
   `/usr/bin/harbour-sukkula`, the engine's
   `/usr/share/harbour-sukkula/lib/libsukkula_ffi.so`, the desktop file, the
   four icon sizes, the four `.qm` catalogues and `qml/`, nothing else, and
   nothing group- or world-writable. The installed binary gets the checks
   of stage 3. With `SUKKULA_ENGINE=rust` the library is the real archive,
   linked as `scripts/cross-build-rust.sh` links the phone's, and must
   export the C ABI and nothing else.

## What only a phone can show

The stubs imitate Silica, Sailfish.Share and Nemo; they are not them.
`docs/MANUAL-TESTS.md` has the device checks; for the shell they are: the
app starts from the launcher and from the Share menu (cold and running),
Silica's own labels render as the stubs assume, the file picker lists
`~/Downloads` under Sailjail, KeepAlive and notifications behave, and the
consent dialog comes up over other pages. The stub's page stack also
assumes what Silica's does: a page it pops is destroyed, and the page
below turns Active once a transition is over. Settings are saved on that
destruction, the next consent dialog waits for it, and a share pops back
to the main page; M-6 and M-17 check them on the phone. The line
drawings (Canvas), the pickers (Sailfish.Pickers, stubs here), the cover
and the tabs' swipe (Silica's PagedView, a stub here) are only laid out
here, never looked at or dragged: M-18, M-19 and M-70 to M-74 are where
they are seen.
