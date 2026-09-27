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
| M-6 | F-C6 | Share a photo from Gallery and a link from the browser, once in Receive mode and once from the History page. With a LocalSend or Quick Share peer on the radar, share a second photo from Gallery. | Sukkula is offered; it opens on the Send tab with the item at the radar's centre, whatever page was open. After the second share the centre holds the new photo only, and the peers stay on the rings (discovery kept running). Nothing is sent until a peer is tapped. |
| M-7 | F-C7 | Rename the device in Settings. | LocalSend and Quick Share peers show the new name; empty falls back to the model name. |
| M-8 | §2 sandbox | Share a photo from Gallery (it lives in `~/Pictures`) and send it. With the file browser, pick and send a file from each of `~/Documents`, `~/Music`, `~/Videos` and a memory card. Then try a file elsewhere in the home directory (put one in `~/.local/share` over SSH and share it from the file manager). | Every one from a granted folder is sent. The one outside them fails with "A file could not be read. …" instead of failing silently. Nothing new appears in any of those folders. |
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
| M-16 | F-C1 | Receive on. In Settings, switch off LocalSend, Quick Share, Magic Wormhole and Bluetooth one at a time, leaving Settings after each; each time, try that protocol both ways: from the desktop `nc -vz <phone-ip> 53317` and a LocalSend send; the Pixel's share sheet; `wormhole send` on the laptop and "Receive with a code"; a Bluetooth send. Then switch them all on again. | A protocol switched off is not on the send radar (no peers with its badge; no Magic Wormhole tile, and no cloud once no internet protocol is left), and nothing reaches the phone over it: 53317 refuses, the phone is not in the Pixel's list, the Receive tab's cloud has no Magic Wormhole tile. The others keep working. Switched on again, each works as before. |
| M-17 | F-C2 | Receive on. Open Settings, change the device name and stay on the page; send a file from the desktop's LocalSend. Accept it, then leave Settings. | The dialog stays until it is answered (Settings being covered restarts nothing) and the file arrives; the new name is used once Settings is left (M-7). |
| M-18 | F-C1, F-C6 | Start the app. Tap Receive and Send at the top, swipe between them, then use the cover's action twice. Turn the phone to landscape on each tab. On the Send tab, glance at the Events view and come back; then leave the app in the background for ten seconds and come back. With a LocalSend, a Quick Share and a paired Bluetooth device around, tap the plus and pick three files in two folders; tap the plus again and add one more; clear them with the cross; pick one again. Tap each peer with a file, then share a text from Notes and tap the Bluetooth device. Tap the cloud, then Magic Wormhole with one file, and receive it with `wormhole receive` on the laptop. Bring nine or more LocalSend devices up at once. Pull each tab's pulley menu. | Send first, underlined; tapping or swiping moves the underline and the mode (Receive: the cover says "Receiving"), and the tab follows the cover. The page stays in portrait. Before a file is chosen the Send tab shows only the plus, the rings and "Tap to choose what to send"; the plus opens the file browser, several files at a time, and the peers and the cloud ("More options") appear with the first file. The glance keeps the peers; after ten seconds away the radar is empty on return and fills again. Bluetooth refuses the text and Magic Wormhole several items, each saying why. A send draws its line, fills its peer and the centre, shows the percentage, and can be cancelled; the wormhole tile shows the code (the QR a tap away) until the laptop connects, then becomes its avatar. The rings are round; as many peers as fit sit on them without covering each other, the rest behind "+N", which lists them all. Each pulley has Settings and History, and no About (it is at the foot of Settings). |
| M-19 | F-C1, F-C2, F-C5 | On the Receive tab, lock and unlock the phone. Send a file from LocalSend, then two files at once from the Pixel with Quick Share; accept each. Decline one. Tap the cloud, then Magic Wormhole, and receive a `wormhole send` from the laptop. Open History from the pulley. | The rings pulse, the phone's name is under the centre, and "Visible over …" names the protocols that are ready (a failed one says why). Each sender comes onto the rings while the dialog asks; accepted, a line runs from it to the centre, both fill, and the percentage and a cancel button are beside it; a few seconds after it ends it leaves. The declined one leaves at once. The wormhole sender comes from Magic Wormhole's tile through the cloud. History lists every transfer, sent and received, and the received texts with Copy; its pulley clears the finished ones. |

## LocalSend

| ID | Covers | Steps | Pass when |
| --- | --- | --- | --- |
| M-20 | F-LS1 | Open LocalSend on the desktop, on iOS and on an Android phone, and leave each on its Receive screen. On the Jolla, with Receive off, open the Send tab and pick a file. Then from the desktop's LocalSend, try to send to the phone while it is still on the Send tab. | Every one appears on the radar within a few seconds, without being refreshed on its side, and each sees the phone. The desktop's send is refused ("not receiving"): the Send tab takes no offers. |
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
| M-64 | F-MW2 (v0.6) | Send a file with Warp on the laptop (or Destiny on an Android phone); on the Jolla, Receive tab, the cloud, Magic Wormhole, Scan QR code, and point the camera at Warp's QR code. The first time, answer the camera prompt. Then do the same from a second Jolla sending with Sukkula, once with its default mailbox and once with a custom one in its Settings. | The viewfinder shows the camera's picture the right way up, and within a second or two of the code filling the square the page closes, the code is in the field and the consent dialog follows; each file arrives intact. The custom mailbox's QR code is received through that mailbox (its log shows the nameplate) although this phone's Settings name the default. |
| M-65 | F-MW2, F-CR2 (v0.6) | On the scan page, point the camera at a QR code of a web address, at a Wi-Fi QR code, then at a croc QR code while on Magic Wormhole's page. Switch croc off in Settings and scan a croc QR code again. Press the power key while the scan page is open, then come back. Deny the camera to Sukkula in Settings, Apps, and open the scan page. | The web address and the Wi-Fi network are only said to be no code (nothing of them is shown or opened), and scanning goes on; the croc code receives over croc; with croc off the receive page says croc is switched off and receives nothing. The camera light goes off when the display does and comes back with the page. Denied, the page says the camera is not available. |

## croc

| ID | Covers | Steps | Pass when |
| --- | --- | --- | --- |
| M-46 | F-CR1 | On the phone, pick two files, tap the cloud, then croc; on the laptop, `CROC_SECRET=<code> croc` with the code the tile shows. Then share a text from Notes and send it over croc the same way. | The tile shows the code as text, and its page the code with a QR code of it; the laptop's croc lists both files, receives them intact (compare SHA-256), and prints the text. The line runs through the cloud and fills. |
| M-47 | F-CR2 | `croc send photo.jpg somefolder/` on the laptop; on the phone, Receive tab, the cloud, croc, type the code (spaces or hyphens). Accept. Then `croc send --text hello` and receive it; then type a wrong code for a third send. | The consent dialog lists the photo and the folder's files, flat, before anything flows; accepted, each arrives intact in `~/Downloads/Sukkula/`. The text lands in History with Copy. The wrong code says so, and the laptop's croc reports a bad password. |
| M-48 | F-CR3 | Run `croc relay --pass s3cret` on the laptop; point Settings' croc relay at it with that password; send and receive once each. Then set a wrong password. | Both transfers go through the laptop's relay (its log shows the room); with the wrong password each fails with "A setting could not be used." |
| M-49 | F-CR4 | With the laptop on the same Wi-Fi, send a 1 GB file to `croc` on the laptop; cancel another half-way from each side. | Data goes through the relay (croc on the laptop says so, never "local"); each cancel stops both sides and leaves no partial file on the phone. |
| M-63 | F-CR1, F-CR2 | The laptop's croc is 11 (`croc --version`) and on the public relays (no `--relay`, no `CROC_RELAY`); send a file each way, and each way with an Android phone's croc app. Then, with croc 10 on the laptop, receive our code and send one of its own. | Every transfer arrives intact; neither the laptop's croc 11 nor the app warns of a "legacy" peer; croc 10 takes our code and its own reaches us. |
| M-66 | F-CR1, F-CR2 (v0.6) | `croc send --qr photo.jpg` on the laptop; on the phone, Receive tab, the cloud, croc, Scan QR code, at the terminal's QR code; once in a dark terminal theme and once in a light one. Then send from a second Jolla over croc and scan its code page's QR code; and scan that QR code with the Android phone's croc app. | croc's `getcroc.com` link is read for its code (the phone opens no web page), in either theme, and the photo arrives intact; the second Jolla's QR code receives too. Whether the Android app reads our QR code (the code on its own) is noted, not required. |

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
