# Manual tests on the Jolla Phone 2026

What no host test can prove: the phone's firewall, its radios, its
sandbox, and real peers. Run the whole list before every release, on a
Jolla Phone 2026 with the Sailfish OS release the package was built for
(5.2 or later), with the package installed from the RPM the release
workflow built. Record the result of every ID in the release notes' test
log; an ID that was not run is a failed ID.

Peers: a current Pixel (stock Android, Quick Share), a Samsung phone
(Quick Share), LocalSend on iOS and on a desktop, the `wormhole` CLI
(Python, current release) on a laptop, and a Bluetooth phone or laptop
paired with the Jolla.

Every ID names the requirement it covers.

## Platform

| ID | Covers | Steps | Pass when |
| --- | --- | --- | --- |
| M-1 | §2 firewall | Receive on. From the desktop, `nc -vz <phone-ip> 53317`; from the Pixel, share to the phone over Quick Share. | Both connect. If connman's firewall drops them, stop: record it, and do not release (spec §2). |
| M-2 | §2 sandbox | `ls -la ~/.local/share/sukkula/sukkula ~/Downloads/Sukkula` after a first run. | Directories `0700`, `localsend-cert.pem`/`localsend-key.pem` and `settings.json` `0600`; nothing of Sukkula's anywhere else in `$HOME`. |
| M-3 | §2 lifecycle | Receive on, close the app from the cover. From the desktop, try to send. | Nothing answers on 53317; no Sukkula process remains (`ps`). |
| M-4 | §2 KeepAlive | Receive a 2 GB file with the screen off. | The transfer completes; with no transfer running the phone suspends as usual. |
| M-5 | §2 cover | Receive on, go to the home screen. | The cover says "Receiving" and shows progress during a transfer. |
| M-6 | F-C6 | Share a photo from Gallery and a link from the browser, once in Receive mode and once from the "What to send" page. With a LocalSend or Quick Share peer on the radar, share a second photo from Gallery. | Sukkula is offered; it opens in Send mode with the item at the radar's centre, whatever page was open. After the second share the centre holds the new photo only, and the peers stay on the rings (discovery kept running). Nothing is sent until a peer is tapped. |
| M-7 | F-C7 | Rename the device in Settings. | LocalSend and Quick Share peers show the new name; empty falls back to the model name. |
| M-8 | §2 sandbox | Share a photo from Gallery (it lives in `~/Pictures`), and pick a file in `~/Documents` with the file picker. | Record whether each can be sent. With only `Downloads` granted, Sukkula shows "Sukkula can read files in Downloads only" instead of failing silently; if the Share-menu photo is unreadable, raise it with the owner (it needs `UserDirs`, a spec change). |
| M-9 | S9 | Run `journalctl --user -f \| grep 'sukkula:'` over SSH. Receive a file over each protocol with debug logging off; turn it on in Settings and receive again, a text too; turn it off. | Off: no line at all unless something failed. On: `DEBUG` lines such as `offer accepted` and `transfer finished`, with counts and sizes only: no file name, text, device name, PIN, code or address in any line. Off again: `debug logging off`, then silence. Nothing of Sukkula's under `$HOME` looks like a log file. |
| M-62 | §3 engine | Start the app from the app grid (the booster `dlopen()`s it), close it from the cover, and start it again with `sailjail /usr/bin/harbour-sukkula` over SSH. Close it, and share a photo from Gallery, so the Share menu starts it (`ExecDBus`). | Each time the main page lists the protocols, never "the engine failed internally", and the console shows no `panicked` line or crash (`docs/FFI.md`, Linking). Started with `SUKKULA_TLS_REPORT=1` in the environment, it prints how many of the reserve's 4096 bytes the GL stack wrote on the GUI thread: record the number. |

## Consent and display

| ID | Covers | Steps | Pass when |
| --- | --- | --- | --- |
| M-10 | F-C2, S5 | Send a file from each receiving protocol. | A consent dialog shows sender, protocol, names, sizes and total before anything is written (`ls ~/Downloads/Sukkula/.partial` is empty until Accept). |
| M-11 | F-C3 | Offer a file and do not answer. | It is declined after 60 s; the sender sees a refusal. |
| M-12 | F-C3 | Offer from three devices at once. | Two dialogs queue; the third sender is refused without a dialog. |
| M-13 | F-C4 | Send a text containing a URL. | Shown as plain text with Copy; nothing opens; the URL is not a link. |
| M-14 | S2 | Rename the desktop's LocalSend alias to `Alice`, then U+202E RIGHT-TO-LEFT OVERRIDE, then `gpj.exe`. | Sukkula shows `Alicegpj.exe`, left to right. |
| M-15 | F-C5 | Cancel a large transfer from each side, in each direction. | Both sides stop; no partial file remains on the phone. |
| M-16 | F-C1 | Receive on. In Settings, switch off LocalSend, Quick Share, Magic Wormhole and Bluetooth one at a time, leaving Settings after each; each time, try that protocol both ways: from the desktop `nc -vz <phone-ip> 53317` and a LocalSend send; the Pixel's share sheet; `wormhole send` on the laptop and "Receive with a code"; a Bluetooth send. Then switch them all on again. | A protocol switched off is not on the send radar (no peers with its badge; no Magic Wormhole tile or cloud), and nothing reaches the phone over it: 53317 refuses, the phone is not in the Pixel's list, "Receive with a code" is gone from Receive mode's menu. The others keep working. Switched on again, each works as before. |
| M-17 | F-C2 | Receive on. Open Settings, change the device name and stay on the page; send a file from the desktop's LocalSend. Accept it, then leave Settings. | The dialog stays until it is answered (Settings being covered restarts nothing) and the file arrives; the new name is used once Settings is left (M-7). |
| M-18 | F-C1, F-C6 | Start the app. Switch to Receive and back to Send, then use the cover's action twice. With a LocalSend, a Quick Share and a paired Bluetooth device around, tap each on the radar with nothing chosen, then with a file, then with a file and a text. Tap Magic Wormhole with one file and receive it with `wormhole receive` on the laptop. Bring nine or more LocalSend devices up at once. | Send mode first, with the peers on the rings and each badge right; Receive shows the protocols' states and the cover says "Receiving"; the switch follows the cover. With nothing chosen the file picker comes first and the send starts once a file is picked; Bluetooth refuses the text and Magic Wormhole two items, each saying why. A send draws its line, fills its peer and the centre, shows the percentage, and can be cancelled; the wormhole tile shows the code (the QR a tap away) until the laptop connects, then becomes its avatar. Seven peers fit on the rings, the rest behind "+N", which lists them all. |

## LocalSend

| ID | Covers | Steps | Pass when |
| --- | --- | --- | --- |
| M-20 | F-LS1 | Open LocalSend on the desktop and on iOS. | Both see the phone, and the phone sees both. |
| M-21 | F-LS2 | Switch the desktop's LocalSend to HTTP (encryption off) and send. | The phone refuses; the desktop reports an error. |
| M-22 | F-LS3 | Send from the phone to the desktop and to iOS. | Files arrive intact (compare SHA-256). |
| M-23 | F-LS4 | Set a PIN; send from the desktop with a wrong PIN, then the right one. | Wrong PIN refused, right PIN reaches the consent dialog. |
| M-24 | F-LS4 | Set a PIN on the desktop's LocalSend, then send to it from the phone. | Known gap: the send fails as refused, because the send command carries no PIN. Record it. |

## Quick Share

| ID | Covers | Steps | Pass when |
| --- | --- | --- | --- |
| M-30 | F-QS1 | Share a file from the Pixel and from the Samsung to the phone, and from the phone to each. | All four arrive intact. |
| M-31 | F-QS2 | The nudge goes out only while Sukkula looks for devices to *send* to, so this is a sending test, run twice. Pixel: Quick Share visible to Everyone (Android keeps that for 10 minutes: set it again before (b) if it has lapsed), its Quick Share screen and share sheet closed, screen on and unlocked. Jolla: Receive off, debug logging on, M-9's `journalctl` running. (a) Settings: Bluetooth nudge **off**. Open Send…, choose Quick Share, watch the list for 60 s, go back. (b) Settings: Bluetooth nudge **on**. Open Send…, choose Quick Share, watch for 60 s. | Record for (a) and for (b) whether the Pixel appeared and after how many seconds. Pass: not in (a), within 30 s in (b), with a `BLE nudge on the air` line in the journal in (b) only. If the Pixel appears in (a), it is announcing itself without the nudge and the run proves nothing: close Quick Share on it, lock and unlock it, and repeat (a) until it stays away before running (b). If it never appears in (b), F-QS2 fails: record the journal's `quickshare:` lines (`no BLE nudge` says why). |
| M-32 | F-QS3 | Share from the Pixel. | The PIN in Sukkula's dialog matches the Pixel's screen. |
| M-33 | F-QS4 | Set visibility to Hidden. | Neither Android phone can see the phone. |
| M-34 | F-QS5 | Share a Wi-Fi network from the Pixel. | Refused; no network change on the phone. |

## Magic Wormhole

| ID | Covers | Steps | Pass when |
| --- | --- | --- | --- |
| M-40 | F-MW1 | Send a file from the phone; receive it with `wormhole receive <code>` on the laptop, and again by scanning the QR code with a phone app. | Arrives intact both ways. |
| M-41 | F-MW2 | `wormhole send file` on the laptop; type the code on the phone. | The consent dialog appears before any data flows; Accept receives it. |
| M-42 | F-MW3 | `wormhole send somefolder/`. | Saved as one archive, unopened. |
| M-43 | F-MW4 | Point Settings at a self-hosted mailbox and relay. | Transfers use them (check the server logs). |
| M-44 | F-MW4 | Point the mailbox at a `wss://` server with a publicly trusted certificate, then at one with a self-signed certificate. | The first works (the system CA bundle is readable inside Sailjail); the second is refused. |
| M-45 | F-MW1 | Send to a laptop on the same LAN, then to one behind another network. | Direct connection on the LAN (the relay's log shows no traffic), relay otherwise; connman lets the outbound connections through. |

## Bluetooth

| ID | Covers | Steps | Pass when |
| --- | --- | --- | --- |
| M-50 | F-BT1 | Send two files to the paired device; cancel a third mid-way. | Two arrive; the third stops on both sides. |
| M-51 | F-BT2 | Send a file *to* the phone over Bluetooth. | The Sailfish system UI handles it; Sukkula is not involved. |
| M-52 | F-BT1 | Send to a phone whose user waits ~45 s before accepting; send to one that declines; send with Bluetooth off. | Accepted late still succeeds; declined shows "refused"; off shows "Bluetooth is off". |
| M-53 | F-C5 | Cancel a Bluetooth send mid-way, then run `busctl --user tree org.bluez.obex`. | No session is left behind. |

## Release

| ID | Covers | Steps | Pass when |
| --- | --- | --- | --- |
| M-60 | M5 | Switch the phone's language to Finnish, German and Swedish. | Every page is translated; nothing is cut off. |
| M-61 | §2 | Install the release RPM over the previous release. | Settings and the certificate survive; peers still pin to the same fingerprint. |
