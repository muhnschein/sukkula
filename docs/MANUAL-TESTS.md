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
| M-5 | §2 cover (v0.7) | With nothing going on, go to the home screen: check the cover. Tap its right-hand action, go home, then its left-hand one. Receive a large file from the desktop and go home while it comes; then have the Pixel offer a file while the app is in the background within 5 s of leaving it. | Nothing going on: "Send" and "Receive", each over its own action, and no name anywhere. The right action opens the app on the Receive tab, the left one on the Send tab, whatever page was open. While the file comes: one ring with the percentage in it, "Receiving", and how many files. The offer: "Offer waiting", "Tap to see it", the seconds left; tapping the cover opens the app on the consent dialog. |
| M-6 | F-C6 | Share a photo from Gallery and a link from the browser, once on the Receive tab and once from the History page. With a LocalSend or Quick Share device listed, share a second photo from Gallery. | Sukkula is offered for the photo, and not for the link (it sends files only, spec v0.7). It opens on the Send tab with the photo chosen ("IMG_….jpg"), whatever page was open. After the second share only the new photo is chosen, and the devices stay listed (discovery kept running). Nothing is sent until a device is tapped. |
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
| M-16 | F-C1 | Receive on. In Settings, switch off Android phones (Quick Share), Computers and other phones (LocalSend), Magic Wormhole and Bluetooth one at a time, leaving Settings after each; each time, try that protocol both ways: from the desktop `nc -vz <phone-ip> 53317` and a LocalSend send; the Pixel's share sheet; `wormhole send` on the laptop and the Receive tab's "Scan a code", Enter code; a Bluetooth send. Then switch them all on again. | A protocol switched off is used by nothing on the Send tab (its devices' rows lose it, or go; with Magic Wormhole and croc off, "Far away" goes), and nothing reaches the phone over it: 53317 refuses, the phone is not in the Pixel's list, the scan page refuses a Magic Wormhole code, typed or scanned, saying why, and with both code protocols off the Receive tab has no "Scan a code". "How others can reach this phone" says Off for each. The others keep working. Switched on again, each works as before. |
| M-17 | F-C2 | Receive on. Open Settings, change the device name and stay on the page; send a file from the desktop's LocalSend. Accept it, then leave Settings. | The dialog stays until it is answered (Settings being covered restarts nothing) and the file arrives; the new name is used once Settings is left (M-7). |
| M-18 | F-C1, F-C6 (v0.7) | Start the app. Tap Receive and Send at the top, swipe between them. Turn the phone to landscape on each tab. On the Send tab, glance at the Events view and come back; then leave the app in the background for ten seconds and come back. With a LocalSend, a Quick Share and a paired Bluetooth device around, and one phone with both LocalSend and Quick Share: tap Photos and pick three photos; + and add a fourth; the cross, then undo in the remorse; the cross again and let it run. Try Videos, Documents and Any file (two files in two folders). Tap each device with a file; long-press the phone with both and send with the other; open About this device. Tap "Send with a code" with one file, then with two. Pull each tab's pulley menu. | Send first, underlined; the page stays in portrait. With nothing chosen the tab asks "What would you like to send?" with four tiles, each opening the platform's own picker (Gallery for photos and videos), and the foot says who is nearby. The glance keeps the devices; after ten seconds away the list is empty on return and fills again. Chosen: "3 photos" with the size; the remorse undoes the cross. Devices are listed by name, the protocol only in the grey line; the phone with both is one row whose menu offers both ways and About this device (its address, and for LocalSend the certificate fingerprint the desktop's LocalSend shows). A send shows in its row, with the percentage, a cross, a line, and how long is left; the other rows wait; after it the files stay chosen. The code page picks Magic Wormhole for one file and croc for two. Each pulley has Settings and History, and the Send tab's Add more and Start over; no About (it is at the foot of Settings). |
| M-19 | F-C1, F-C2, F-C5 (v0.7) | On the Receive tab, lock and unlock the phone. Send a file from LocalSend, then three photos at once from the Pixel with Quick Share; accept each. Decline one. Tap "Scan a code", then Enter code, and receive a `wormhole send` from the laptop with its code. Unfold "How others can reach this phone". Leave the app in the background for ten seconds, then come back. Open History from the pulley. | "Ready to receive", the rings pulsing, "Nearby, this phone shows up as" and the name. The consent dialog shows the sender, "wants to send you 3 photos over Quick Share", the PIN and each photo with a photo icon (never a picture of it). Accepted, "Receiving" shows the sender, the percentage and a cross; a few seconds after it ends it moves to "Received today" as one row, "3 photos", whose page lists them. The scan page asks no protocol and goes once the offer is answered. The unfolded rows say Ready for each way, with its setting (visible to everyone, no PIN). After ten seconds away the desktop's LocalSend no longer finds the phone; back on the Receive tab it does again. History lists every transfer and the received texts with Copy. |
| M-70 | §2 (v0.7) | Grant Sukkula's permissions when first asked (Gallery's pickers ask for the media index). Tap Photos, Videos and Documents in turn; pick from a memory card in each. | Each opens the platform's picker with what the media index lists; the chosen files are sent (M-8's rule: only from the granted folders). Denied, the pickers show nothing, and Any file still works. |
| M-71 | §2 lifecycle (v0.7) | Receive tab, then the home screen for ten seconds; from the desktop, try `nc -vz <phone-ip> 53317` and a LocalSend send, and the Pixel's share sheet. Then have the desktop offer a file within 5 s of leaving the app, and leave it unanswered for ten seconds; answer it. Then start receiving a large file and leave the app for a minute. | Ten seconds away: nothing answers and the phone is in nobody's list. The offer made in time stays until answered (the cover says it waits) and receiving stops once it is. The large file arrives whole; receiving stops after it. |
| M-72 | F-C2 (v0.7) | Offer one PDF from the desktop's LocalSend, then 60 photos from the Pixel. | One file: the document icon big, its name and size, "wants to send you a file over LocalSend". Sixty: "60 photos", the first 50 listed with photo icons, "and 10 more files", the total in the header. No picture of any photo anywhere. |
| M-73 | F-C1 (v0.7) | In Settings, check the groups "Nearby" and "Far away"; switch Android phones off and on; unfold "Your own servers"; type a bad mailbox URL and fold the section. | Visible to and the Bluetooth nudge grey out while Android phones is off, and the PIN while Computers and other phones is off; the servers come unfolded while one is wrong, and the page does not leave with it. |
| M-74 | F-C1 (v0.7) | Switch Magic Wormhole and croc off, then Android phones and Computers and other phones too. Look at the Receive tab each time. | "Ready for codes" with only the codes left; "Receiving is switched off in Settings." with nothing. |

## LocalSend

| ID | Covers | Steps | Pass when |
| --- | --- | --- | --- |
| M-20 | F-LS1 | Open LocalSend on the desktop, on iOS and on an Android phone, and leave each on its Receive screen. On the Jolla, with Receive off, open the Send tab and pick a file. Then from the desktop's LocalSend, try to send to the phone while it is still on the Send tab. | Every one is listed under "Nearby" within a few seconds, without being refreshed on its side, and each sees the phone. The desktop's send is refused ("not receiving"): the Send tab takes no offers. |
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
| M-64 | F-MW2 (v0.6) | Send a file with Warp on the laptop (or Destiny on an Android phone); on the Jolla, Receive tab, tap "Scan a code" and point the camera at Warp's QR code; tap the viewfinder once on the code. The first time, answer the camera prompt. Then do the same from a second Jolla sending with Sukkula, once with its default mailbox and once with a custom one in its Settings. | The viewfinder fills the page under its header, the camera's picture the right way up and sharp within a couple of seconds (the tap focuses on the code), and within a second or two of the code filling a third of it the consent dialog follows, and the page goes once it is answered; each file arrives intact. The custom mailbox's QR code is received through that mailbox (its log shows the nameplate) although this phone's Settings name the default. |
| M-65 | F-MW2, F-CR2 (v0.6) | On the scan page, point the camera at a QR code of a web address, at a Wi-Fi QR code, then at a croc QR code. Tap Enter code with a shopping list on the clipboard, then with a croc code copied. Switch croc off in Settings and scan a croc QR code again, then type one. Press the power key while the scan page is open, then come back. Deny the camera to Sukkula in Settings, Apps, and open the scan page. | The web address and the Wi-Fi network are only said to be no code (nothing of them is shown or opened), and scanning goes on; the croc code receives over croc; Enter code opens a field over the viewfinder, which keeps reading, empty for the shopping list and holding the copied code; with croc off the page says croc is switched off, for the scanned and the typed code, and receives nothing. The camera light goes off when the display does and comes back with the page. Denied, the page says the camera is not available, and Enter code still works. |

## croc

| ID | Covers | Steps | Pass when |
| --- | --- | --- | --- |
| M-46 | F-CR1 | On the phone, pick two files and tap "Send with a code"; on the laptop, `CROC_SECRET=<code> croc` with the code the page shows. Then, with one file, open the page (Magic Wormhole), switch "Their app" to croc and receive the new code on the laptop. Tap Share… and pick Notes. | The page picks croc for two files and shows its code and QR code; the laptop's croc lists both files and receives them intact (compare SHA-256); once it has come, the page shows the progress, and so does the Send tab's "Far away" row. Switching gives the Magic Wormhole code up and shows a croc one. Notes gets the code as text, nothing else. |
| M-47 | F-CR2 | `croc send photo.jpg somefolder/` on the laptop; on the phone, Receive tab, "Scan a code", Enter code, type the code (spaces or hyphens; once with a capital letter). Accept. Then `croc send --text hello` and receive it; then type a wrong code for a third send. | The consent dialog lists the photo and the folder's files, flat, before anything flows; accepted, each arrives intact in `~/Downloads/Sukkula/`. The text lands in History with Copy, and under "Received today" as a text message. The wrong code says so, and the laptop's croc reports a bad password. |
| M-48 | F-CR3 | Run `croc relay --pass s3cret` on the laptop; point Settings' croc relay at it with that password; send and receive once each. Then set a wrong password. | Both transfers go through the laptop's relay (its log shows the room); with the wrong password each fails with "A setting could not be used." |
| M-49 | F-CR4 | With the laptop on the same Wi-Fi, send a 1 GB file to `croc` on the laptop; cancel another half-way from each side. | Data goes through the relay (croc on the laptop says so, never "local"); each cancel stops both sides and leaves no partial file on the phone. |
| M-63 | F-CR1, F-CR2 | The laptop's croc is 11 (`croc --version`) and on the public relays (no `--relay`, no `CROC_RELAY`); send a file each way, and each way with an Android phone's croc app. Then, with croc 10 on the laptop, receive our code and send one of its own. | Every transfer arrives intact; neither the laptop's croc 11 nor the app warns of a "legacy" peer; croc 10 takes our code and its own reaches us. |
| M-66 | F-CR1, F-CR2 (v0.6) | `croc send --qr photo.jpg` on the laptop; on the phone, Receive tab, "Scan a code", at the terminal's QR code; once in a dark terminal theme and once in a light one. Then send from a second Jolla over croc and scan its code page's QR code; and scan that QR code with the Android phone's croc app. | croc's `getcroc.com` link is read for its code (the phone opens no web page), in either theme, and the photo arrives intact; the second Jolla's QR code receives too. Whether the Android app reads our QR code (the code on its own) is noted, not required. |

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
