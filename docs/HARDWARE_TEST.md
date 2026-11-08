# First hardware test: NanoPC-T6N from microSD

This boots the image from a microSD card. It does not change the eMMC, and it only adds a
`cuprated` directory to the existing `NODO_BLOCKCHAIN` SSD partition.

## What this image has never done

It passed every test under emulation, but it has never run on real hardware. These parts are
unproven on the T6N:

- The FriendlyElec bootloader starting from the card
- The vendor 6.1 kernel with Void Linux
- Ethernet with the in-kernel Realtek driver
- The NVMe drive and the XFS mount
- The firewall on this kernel

## Before you start

On the current Nodo system, check there is at least 300 GB free, then shut it down:

```sh
df -h /media/monero
sudo shutdown -h now
```

## Flash the card

Use a microSD card of 8 GB or more. Find its device name on your PC:

```sh
lsblk -o NAME,SIZE,TRAN,MODEL,MOUNTPOINT
```

**Warning: `dd` erases the target device completely, and this cannot be undone.** Check that the
size and model match the card, not an internal drive. Replace `/dev/sdX` with the card's name:

```sh
sha256sum out/images/nanopc-t6n/monokulo-nanopc-t6n-sd.img
cat out/images/nanopc-t6n/monokulo-nanopc-t6n-sd.img.sha256
sudo dd if=out/images/nanopc-t6n/monokulo-nanopc-t6n-sd.img of=/dev/sdX bs=4M conv=fsync status=progress
sync
```

The two checksums must match.

## Boot it

1. Insert the card into the Nodo, connect Ethernet, and power it on.
2. Wait about three minutes. The first boot grows the root partition to fill the card.
3. Find the IP address in one of these places:
   - The Nodo's screen, if the console appears there. It shows the address and the SSH command.
   - Your router, as a device named `monokulo-nodo`.
4. Log in. Only your `id_ed25519` key works, and there is no password login:

```sh
ssh root@<device-ip>
```

## If you can log in

Run this and send the output:

```sh
nodo-report
```

It is read-only and contains no keys or secrets.

## If you cannot log in

Every boot saves the same report to the card twice: about 20 seconds after boot, and again two
minutes later. Power off, move the card to your PC, and read it without mounting. Replace `sdX`
with the card's name:

```sh
sudo debugfs -R 'cat /var/log/monokulo-diag/latest.txt' /dev/sdX8
```

If that file doesn't exist, the system never got far enough to start services. Check whether the
root partition was resized, which shows that the bootloader and kernel at least ran:

```sh
sudo debugfs -R 'stat /etc/fs.resized' /dev/sdX8
```

If the Nodo started its old system with the card inserted, the board didn't boot from the card.
Power off, remove the card, and report what happened.

## Going back

Power off and remove the card. The Nodo should start its original system from the eMMC.

## What to report

- Whether it booted, and how you could tell
- The `nodo-report` output, or the `latest.txt` file from the card
- Anything shown on the screen
