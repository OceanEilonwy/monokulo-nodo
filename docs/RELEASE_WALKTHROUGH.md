# Release walk-through on a NanoPC-T6N

A scripted hour with the real board, run before this project's packages reach owners in a system
snapshot. Someone installs, configures, publishes and updates an app through the real TUI on the
device, and the result is a report that the release workflow checks.

## Why this exists

The VM journeys (`tests/tui-e2e.sh`, `tests/tui/journeys.py`) drive the real nodo-tui against the
real helper. They still missed bugs that owners found on the board within minutes:

- an install showed no progress, and lost its result when you pressed Esc;
- "Check now" gave no sign that it was doing anything;
- an empty optional number setting blocked saving the whole form;
- clicks on a settings form did nothing once the form was scrolled;
- adding a domain provider whose key sees only some of the account's domains gave a misleading
  message.

A VM cannot cover the board itself: its bootloader, kernel and screen, real SSH clients and
terminal sizes, a real mouse, your router, public DNS, Let's Encrypt, the feeds over the internet,
USB sticks, reboots and power. This walk-through covers those. The steps that check one of the bugs
above say so, so a regression is caught where it was found.

[HARDWARE_TEST.md](HARDWARE_TEST.md) was the first boot test, from two kept images. It still holds
the details on flashing a card and reading the diagnostic report off a card that does not boot.
This document is for every release after that.

## When it is required

`.github/workflows/system.yml` publishes a snapshot on every push to `main` that changes a base
package, and every Monday. Before it signs one, `scripts/walkthrough.sh gate` compares the
snapshot's packages with the snapshot already published:

- If none of this project's packages changed (the weekly run that only refreshes Void's index),
  it publishes as before.
- If any changed, it publishes only when `releases/walkthroughs/` holds a report from this
  walk-through that passed, is at most 30 days old, and ran on a device with every changed package
  at exactly the snapshot's version. Otherwise the run fails and says why.

So a merge that raises a package's revision waits on `main` until someone walks it through on
hardware. Several merges can share one walk-through: build the image from the newest commit.
While one waits, the weekly snapshot waits too, because it carries the same new packages; Void
drops old packages from its mirror after a while, so don't let a release wait for weeks.

In an emergency (a security fix that cannot wait for the board), run the system workflow by hand
with `walkthrough_waiver` set to the reason. It publishes, and the reason goes in the run's summary
and the log. Walk the release through afterwards anyway.

## Before the hour

None of this is timed. Do it the day before if you can.

1. **Build the candidate image** from the commit you want to release, on a clean checkout:

   ```sh
   git status --short   # must print nothing
   scripts/build-packages.sh aarch64 $(sed -e 's/#.*//' -e '/^[[:space:]]*$/d' feed/base-packages)
   scripts/build-image.sh nanopc-t6n-mainline ~/.ssh/id_ed25519.pub
   ```

   The image is `out/images/nanopc-t6n-mainline/monokulo-nanopc-t6n-mainline-sd.img`. Its packages
   have the same versions CI builds from the same recipes, which is what the gate compares.

2. **Flash a fresh microSD card** with it (8 GB or more), as in
   [HARDWARE_TEST.md, "Flash the cards"](HARDWARE_TEST.md#flash-the-cards). Label it with the
   commit. Keep the previous walk-through's card as it is, labelled: it is the card for the
   after-release steps (P01 to P03).
3. **Have ready:**
   - The NanoPC-T6N with its NVMe drive (`NODO_BLOCKCHAIN`), Ethernet and its screen.
   - A USB stick formatted FAT32, exFAT or ext4, that may be written to.
   - A domain you control at a provider the TUI can connect (Web › Domain providers › Add lists
     them), with **at least two domains** on the account and an API key that sees them. For the
     optional W15, a second key limited to some of the domains (Cloudflare tokens can be limited
     to zones, for example).
   - Port forwarding for 80 and 443 to the Nodo on your router, or the ability to set it up.
   - A phone with mobile data, as the "outside" network, and its hotspot for the laptop.
   - The handicrafts feed's key fingerprint, from its publisher by a way you trust (not from the
     feed itself): <https://github.com/OceanEilonwy/handicrafts>. Handicrafts has about 27
     settings, with optional numbers and secrets, which is why it is the walk-through app.
   - A laptop with this checkout, `ssh`, `curl`, `openssl` and ideally `dig`, and a terminal
     that can be set to exactly 80x24, can be made large, and passes mouse events (most do).
   - A second device on the home network with a browser.
4. **Decide the passphrases** you will use (device passphrase, backup passphrase of 12 characters
   or more, a 6-digit screen PIN). Never type any of them into the report or the script.

## Timing

| Part | Steps | Minutes |
|---|---|---|
| Boot, first run, terminal and mouse | W01 to W05 | 10 |
| Feeds, install, settings | W06 to W10 | 15 |
| Updates | W11 to W13 | 5 |
| Domains and publishing | W14 to W18 | 12 |
| Touchscreen, reboot, power | W19 to W21 | 10 |
| Backup and restore, undo | W22 to W25 | 8 |
| The script and the report | A01 to A19 | 5 |
| Optional | W13, W15, W24, W26, W27 | 5 to 30 more |

## The script

`scripts/walkthrough.sh` does what a laptop can check over SSH. It only reads from the device:
`xbps-query`, `sv status`, read requests through `nodo-tui --ctl`, and `nodo-report`. The one
exception is `--check-updates`, which asks the helper to check for updates as Check now does, and
so records the time of that check on the device. It never asks for or sends a passphrase.

Run it at the end, after W25 and before the optional W27, so the app is installed, restored and
still public. Run it from the same terminal you used for the TUI, on the home network:

```sh
scripts/walkthrough.sh run <device-address> --site <public name> --local-site <app>.local --check-updates --manual
```

`--manual` then asks each manual step below: `p` passed, `f` failed (with a few words on what
went wrong), `s` skipped (optional steps only), or Enter to leave it open. Answer from the notes
you took while doing them. `scripts/walkthrough.sh record <report>` asks the open ones again later.
Then switch the laptop to the phone's hotspot and run A19 from outside:

```sh
scripts/walkthrough.sh outside <public name> out/walkthrough/<report>.txt
```

It writes two files to `out/walkthrough/`:

- `<date>-release.txt`, **the report**: each step's result, the board, kernel, installed snapshot
  and package versions, and the commit of your checkout. It names no address, host or domain, so
  it can be committed.
- `<date>-release.log`, **for you only**: the helper's answers and `nodo-report`, with any line
  that looks like it could hold a secret left out. It has your addresses; don't publish it.

| Step | What the script checks | Needed |
|---|---|---|
| A01 | The laptop has `ssh`, `curl` and `openssl` | yes |
| A02 | The size of this terminal, and that an SSH session gets the same size, at least the TUI's minimum | yes |
| A03 | SSH logs in with a key and no password, and a command runs without opening the TUI | yes |
| A04 | `/proc/device-tree/model` is a NanoPC-T6N, and the kernel has Landlock (the mainline card) | yes |
| A05 | Every package in `feed/base-packages` is installed at the version this checkout's recipe builds | yes |
| A06 | `sv status`: every enabled service is running | yes |
| A07 | The helper answers; the device is set up, unlocked, and has an SSH key | yes |
| A08 | The health monitor answers | yes |
| A09 | System updates: a server is set, a check has run, no error is recorded; the installed snapshot | yes |
| A10 | At least two feeds (the default and the one from W07), none with a problem or a changed key | yes |
| A11 | The walk-through app (`--app`, default handicrafts) is installed and running | yes |
| A12 | `nodo-report` runs to its last section (kept in the log) | yes |
| A13 | `--check-updates`: the system and app update checks both answer | optional |
| A14 | After a kernel update, `/var/lib/nodo/boot/previous` holds the old kernel and device tree | optional |
| A15 | A public resolver (1.1.1.1, or `WALKTHROUGH_RESOLVER`) has a record for the public name | yes |
| A16 | The device serves the public name over HTTPS with a certificate the laptop trusts | yes |
| A17 | That certificate is from Let's Encrypt's production service (not staging) with 14 days or more left | yes |
| A18 | The home network name answers over HTTPS (the device's own CA is not checked) | yes |
| A19 | From outside: the name resolves, answers over HTTPS with a trusted certificate from Let's Encrypt | yes |

A16 to A18 connect to the device's own address, so they work inside the home network whether or
not your router sends traffic for its public address back in. A19 is the visitor's view.

## The steps

Each step says what to do, what you must see, and when it fails. Keys are the TUI's: `1` to `4`
open Home, Apps, Web and System, `Tab` moves between tabs, `?` is help, `Esc` goes back, `q` quits
to the shell (type `nodo-tui` to open it again). Write down anything odd even when the step passes.

### W01 · Boot the candidate card

Insert the card, connect Ethernet, power on.

- See: the boot splash on the screen, then within about three minutes the device's address on the
  screen; the system LED blinks in a heartbeat.
- Fails if: the screen stays black or shows only the bootloader logo, or no address appears. Use
  [HARDWARE_TEST.md, "If you cannot log in"](HARDWARE_TEST.md#if-you-cannot-log-in).

### W02 · First run over SSH at 80x24

Set the terminal to exactly 80 columns and 24 rows (`printf '\033[8;24;80t'` works in most
terminals; check with `stty size`). Then `ssh root@<address>`.

- See: the TUI opens by itself on Home › Set up this device, Step 1 of 2.
- Type a 7-character passphrase and Enter: "Use at least 8 characters." Clear it (Ctrl-U), type the
  real one and Enter, then a different one at "Again": "The two passphrases differ. Type them
  again." Type it twice correctly: the SSH keys step lists your key; Esc opens Home.
- Fails if: the passphrase is shown in clear, any rule is not in plain words, or Home does not open.

### W03 · Keyboard at 80x24

On every tab (`1` to `4`, then `Tab` and `Shift-Tab`) and one level into each list (Enter, then Esc):

- See: the tab strip, the page and the key line at the bottom all fit; no word is cut off mid-word
  except list descriptions ending in "…"; a page that does not fit says "↓ N more — scroll" and `↓`
  reaches the end; `?` opens Help and Esc closes it.
- Fails if: anything overlaps, a key line is cut off, or a page has content you cannot reach.

### W04 · A large terminal, and resizing

Maximise the terminal (200 columns or more if the screen allows) with the TUI open, then make it
small again, then smaller than 60x16, then back to 80x24.

- See: each size redraws at once without leftovers; below 60x16 it says it is too small and only
  `q` works; back at 80x24 it is where you left it.
- Fails if: garbage remains, it crashes, or it ends.

### W05 · Mouse

Click each tab name, a row on Apps, a breadcrumb part (such as "Apps" in Apps › …), and a button;
scroll with the wheel on a long page; move the pointer over clickable things.

- See: each click does what Enter on it would; the wheel scrolls; what can be clicked gets a hover
  fill.
- Fails if: a click does nothing or hits the wrong row.

### W06 · The project's feed lists its apps

Apps (`2`) › Get apps (`/`).

- See: within a minute, the apps of the default `monokulo` feed (such as the light wallet server
  and Kringle), read from <https://nodo.monokulo.com/feed>; Sources (`s`) shows `monokulo`, On.
- Fails if: the list stays empty without saying why, or Sources shows a problem.

### W07 · Add a third-party feed and check its key

Get apps › Sources (`s`) › Add a source (`a`), paste
`https://github.com/OceanEilonwy/handicrafts/releases/download/feed`, Enter.

- See: the feed's name, how many apps it offers, and "Its key" as groups of hex digits. These are
  the same digits as the publisher's fingerprint (`aa:bb:…`), grouped differently. Compare every
  group with what you got from the publisher.
- Press Enter on the default, "▸ Cancel": nothing is added. Add it again, choose Add source with
  `→` and Enter: Sources lists it, On, with its app count.
- Fails if: the default is not Cancel, the key differs from the publisher's, or the address
  (a GitHub release download, which redirects) cannot be read.

### W08 · Install an app with a long settings form

Get apps, search `handicrafts`, Enter to read it, `i` to install.

- See: the review lists what it will be able to do, says it comes from a source you added, and
  starts on "▸ Cancel". Choose Install. If it asks for setup values or the device passphrase, give
  them.
- See: "Installing handicrafts Step k of N" with a percentage, and the steps ticking off as they
  happen (this regressed once: no progress at all).
- While it runs, press `1` (Home), wait a few seconds, then go back through Apps (`2`) to
  handicrafts. See: the install still going, or its result. Do not press Esc on the install page
  while it runs: that cancels it.
- See at the end: "Installed and healthy." Press Esc: Apps lists handicrafts as Running (this
  regressed once: the result was lost on Esc).
- Fails if: there is no visible progress, the result is lost, or it is not running afterwards.

### W09 · Fill, clear and save settings

At 80x24, Apps › handicrafts › Settings.

1. Pick an optional number setting, Enter, type a valid value, Enter: "1 change, not saved yet"
   (or similar). Save with `Ctrl-S`: it restarts the app, and the page then says there are no
   unsaved changes.
2. The same setting again: Enter, `Ctrl-U` to clear it, Enter, `Ctrl-S`. See: it saves with the
   setting empty (this regressed once: an empty optional number blocked saving).
3. Type an invalid value into a number (letters, or out of its range), Enter, `Ctrl-S`. See: the
   field is marked with the reason in plain words, and "Fix the marked field before saving.
   Nothing was changed."; `z` undoes it.
4. A secret setting: Change, type a test value. See: bullets only. Save. See: the value is never
   shown again, only bullets.
5. Esc, then open Settings again. See: the saved values, the optional number still empty.

- Fails if: a valid form cannot be saved, an error is not in plain words, or a secret is ever shown.

### W10 · Click fields on a scrolled settings form

Still at 80x24 on the settings form: scroll to the bottom with the mouse wheel, then click a field
near the bottom; scroll halfway back with `↑` and click another.

- See: the field you clicked is the one selected and edited (this regressed once: clicks on a
  scrolled form did nothing).
- Fails if: a click selects nothing or the wrong field.

### W11 · Check now with the network

System (`4`) › Updates › Check now (`r`, or click "Check now ›").

- See: right away, a sign that it is checking (this regressed once: no feedback at all), then the
  result: the updates found, or that there are none, with the time of the last check updated.
- Fails if: nothing visibly happens, or it never finishes.

### W12 · Check now without the network

Unplug the Ethernet cable (and if Wi-Fi was joined, turn it off under Home › `w`). Check now.

- See: a plain error saying the Nodo could not reach the update server, not "up to date" and not
  silence. Plug the cable back in, wait for the address, Check now: it succeeds.
- Fails if: it claims success offline, says nothing, or does not recover.

### W13 · Apply an update that is offered, then undo it (optional)

Only if System › Updates offers something (a candidate card usually has the newest packages, so the
Nodo row offers nothing; an app may still have an update). Tick an app with Space, Enter to update.

- See: progress, then the app back on its new version and healthy. Then System › Updates › Undo an
  update: the app with its previous version; Enter, confirm (it starts on Cancel): "✓ … is back
  on …".
- Fails if: an update or undo fails, or the app does not come back healthy.

### W14 · Connect a domain provider that has several domains

Web (`3`) › Domain providers (`p`) › Add. Pick your provider, paste the API key (and secret, if it
has one).

- See: "✓ … is connected" and "Apps can now use these web addresses:" with **every** domain the key
  can see, at least two. The key is never shown again.
- Fails if: a domain is missing, or the key is shown.

### W15 · A provider key that sees only some domains (optional)

Add the provider again with the second key, the one limited to some of the domains.

- See: the domains that key can see, and nothing that says the key is wrong or the other domains
  are broken (this regressed once: a misleading message).
- Fails if: the message blames the key or the domains it cannot see, or is not true.

### W16 · Put the app on the home network

Apps › handicrafts › Hosting. Tick Home network (Space), Check and save.

- See: the address `https://handicrafts.local` (or under your domain, if Web › `d` chose that for
  local names). Open it in a browser on the second device on the home network: the app's page. With
  the device's own CA, the browser warns until you trust it; that is expected.
- Fails if: the page does not open from another device.

### W17 · Put the app on the internet

First, Web › `d`: check "Use the staging server" is not ticked (staging certificates are not
trusted). Then Apps › handicrafts › Hosting, tick Internet, pick the provider and domain from the
list (all domains of all providers are there), type an address.

- Check and save. See: a list of checks with ✓ (Domain and API key work, Public IP found, …). If a
  port is not reachable, it says which and how to forward it; set up the router and press `r`.
- See: "Ready to put handicrafts on the internet" with what that means, starting on Cancel. Choose
  Put online. See: "Getting its secure connection…", then online within a few minutes.
- Fails if: a check fails without a reason, the warnings are not shown, or no certificate arrives.

### W18 · Open the public site from outside

On the phone with Wi-Fi off (mobile data), open `https://<public name>`.

- See: the app's page, a padlock, and in the certificate details an issuer of Let's Encrypt.
- Then switch the laptop to the phone's hotspot and run `scripts/walkthrough.sh outside` (A19).
- Fails if: it does not open from outside, or the browser warns.

### W19 · Touchscreen

On the built-in screen:

- See: the tiles, OLED dark. Tap the gear (Manage): the same four tabs as the TUI. Browse each; it
  only looks until unlocked.
- Over SSH, in the shell (`q` leaves the TUI): `nodo-tui --ctl screen-pin`, then type the device
  passphrase and a 6-digit PIN at the two `Secret:` prompts. On the screen, try a change: it asks
  for the PIN; a wrong PIN is refused, the right one unlocks.
- Arrange: drag a tile somewhere else, and see it stay there. ⏻ Shut down: "Shut down the Nodo?"
  with Cancel and Shut down; tap Cancel.
- See throughout: every tap lands on what is drawn under your finger (the picture is turned a
  quarter, and the touches must be too).
- Fails if: a tap lands elsewhere, a change goes through without the PIN, or Cancel shuts down.

### W20 · Reboot and unlock

Over SSH, `reboot` from the shell. Log in again when the address is back.

- See: Home says "Your Nodo is locked", and handicrafts waits. Unlock (`u`): a wrong passphrase is
  refused in plain words ("That passphrase did not unlock this device. Try again."); the right one
  unlocks, and handicrafts runs again. The public site opens from the phone again.
- Fails if: it does not come back, the unlock is unclear, or an app or the site stays down.

### W21 · Shut down from the touchscreen, unplug, power on

⏻ Shut down › Shut down on the screen.

- See: each service stopping, then "Safe to unplug", which stays after the board is off. Unplug,
  wait ten seconds, plug in. It boots; unlock over SSH as in W20.
- Fails if: "Safe to unplug" never comes, or the node or an app is damaged after power on (Home
  says so, or cuprated's log under Apps › cuprated › Logs shows a database error).

### W22 · Back up to a USB stick

Plug in the stick. System › Backups.

- See: "USB drive ·" with the stick's model and its free space. Press `u`, give the device
  passphrase, then a backup passphrase twice. See: the new `.nodobak` in the USB list.
- Unplug the stick, plug it back in, open Backups again: the backup is still listed.
- Fails if: the stick is not found, the backup fails, or a passphrase is ever shown.

### W23 · Restore that backup on this device

System › Backups, `Tab` to the USB list, the backup, `r`.

- Type a wrong backup passphrase: refused. The right one: what the backup holds. Type `restore`.
- See: it finishes, and afterwards handicrafts runs with the settings from W09, its secret still
  works, and the public site still opens.
- Fails if: the restore fails, or anything is missing afterwards.

### W24 · Restore onto a freshly flashed card (optional)

Flash the card again (or a second card), boot it, and at first run press `Ctrl-R` (restore from a
USB backup). Pick the stick and the backup, type the backup passphrase and `restore`, and choose a
device passphrase.

- See: the device comes back unlocked with handicrafts installed from the copy inside the backup,
  its settings and the feed from W07. Reflashing takes about 15 minutes.
- Fails if: anything from the backup is missing.

### W25 · Undo an update: what it offers is true

Home › `r`, or System › Updates › Undo an update.

- See: the apps that can roll back, or that there is nothing to roll back, matching what you
  updated in W13; under System, "The system runs the version it was updated to. No fallback
  happened."
- Fails if: it offers something that was not updated, or the System section is wrong.

### W26 · Kernel recovery drill (optional)

This is the only way back from a kernel update that does not boot: the SD card has no second slot
(UPDATES.md, "Kernel updates on the T6N"). Only on a card where A14 passed. About 15 minutes.

1. Over SSH, copy the saved partitions off the device:
   `scp root@<address>:/var/lib/nodo/boot/previous/kernel.part root@<address>:/var/lib/nodo/boot/previous/resource.part .`
2. Power off from the screen, move the card to the laptop, and find its `kernel` and `resource`
   partitions: `lsblk -o NAME,SIZE,PARTLABEL /dev/sdX`. Check the size and model are the card's.
3. `sudo dd if=kernel.part of=/dev/sdXN bs=1M conv=fsync` for the `kernel` partition, and the same
   with `resource.part` for `resource`.
4. Boot the card. See: `uname -r` shows the previous kernel. Then apply the snapshot again (or
   write the new kernel back the same way) so the card matches the release.

- Fails if: the card does not boot the old kernel.

### W27 · Take the site off the internet (optional)

Apps › handicrafts › Hosting, untick Internet, save.

- See: the site no longer opens from the phone on mobile data (after its DNS record expires); it
  still opens on the home network.
- Fails if: it still answers from outside after a few minutes.

## After the release is published

The gate holds a release until it has been walked through, so the candidate's own snapshot can only
reach a device through the update channel after it is published. That path, the one every owner
takes, is checked on the previous walk-through's card within a day of publication. If it fails,
withdraw the snapshot (`scripts/publish-snapshot.sh`, "Withdrawing a bad snapshot") and fix it.

Boot the previous walk-through's card, unlock it, and:

```sh
scripts/walkthrough.sh run <device-address> --after-release --manual
```

A05 then checks that the card now has the released versions. Commit this report beside the other.

### P01 · On the previous walk-through's card, apply the new snapshot

System › Updates.

- See: the Nodo row offers the new snapshot (Check now if not yet), saying whether it needs a
  restart. Update (or Update and restart): progress, then what changed.
- Fails if: it is not offered, is refused, or fails.

### P02 · After the update

- See: the open TUI restarted itself into the new version; it asks to unlock again (the helper
  restarted); after unlocking, every app runs as before; the public site from the previous
  walk-through, if still on, still opens.
- Fails if: the TUI is stuck or old, or an app does not come back.

### P03 · A snapshot that needs a restart (optional)

Required when the snapshot said it needs a restart. Restart from the shell, unlock.

- See: `uname -r` is the new kernel, and A14 passes (the old kernel is kept for W26).
- Fails if: it does not boot (recover with W26), or the old kernel is still running.

## Committing the report

When `scripts/walkthrough.sh verify out/walkthrough/<date>-release.txt` says the walk-through
passed, copy the report to `releases/walkthroughs/` (keep its name, which starts with the date)
and open a pull request with only that file. Read it first: it should hold no address, host
name or anything personal, and the notes you typed are in it word for word. Once it is on `main`,
run the system workflow by hand (Actions › system › Run workflow); the gate finds the report and
the snapshot is published. Attach the same file to any announcement of the release.

## If a step fails

Record it as failed with a few words, finish the other steps if you can (one report with every
problem is worth more than a report that stops at the first), and open an issue with the report
and the relevant part of the log. A failed report does not open the gate; after the fix, walk
through again from a new image. A failed optional step fails the walk-through too: it found a bug.

## What this does not cover

- The vendor 6.1 card (`nanopc-t6n`): releases are built and tested for the mainline card.
- eMMC, A/B slots and the boot fallback, which are not built yet (UPDATES.md, section 3).
- Wi-Fi and Bluetooth with real radios, Tor, a data drive filled by a full sync, and the
  automatic update window: covered by the VM journeys, and worth a look on hardware when they
  change.
- That a withdrawn snapshot moves a real device back: the publisher side is tested in the VM
  (`tests/snapshot-update.sh`); on hardware it would mean publishing a withdrawal.
