# First hardware test: NanoPC-T6N from microSD

This boots the image from a microSD card. It does not change the eMMC, and it only adds a
`cuprated` directory to the existing `NODO_BLOCKCHAIN` SSD partition.

There are two cards to try in one sitting. Test the mainline card first. If it does not boot, or
something on it is wrong, test the 6.1 card the same way. Both use the same bootloader, the
same partitions and the same Void Linux system; only the kernel differs. See
[MAINLINE_KERNEL.md](MAINLINE_KERNEL.md) for why.

| Card | Image | SHA-256 |
|---|---|---|
| 1, mainline 6.18 (try first) | `out/hardware-test-1/monokulo-nanopc-t6n-mainline-sd.img` | `8fb8edafd9abf60a01e293b9034432a5440052226defe20d5a65898a95c17be3` |
| 2, vendor 6.1 (fallback) | `out/hardware-test-1/monokulo-nanopc-t6n-sd.img` | `485933f5c39e2d2242457910352d85238e6bda7e112df03e105eca26ad0ac9b4` |

These two images were built from commit `b666687`, before the platform packages, the confined
cuprated and the restricted loopback RPC were merged. They are kept in `out/hardware-test-1/` so
that later builds, which write to `out/images/`, cannot replace them. Images built from a later
commit have different checksums and are not the ones this test describes.

Label the cards so they cannot be mixed up. Both accept only your `id_ed25519` key.

## What these images have never done

They passed every test under emulation, but neither has run on real hardware. These parts are
unproven on the T6N:

- The FriendlyElec bootloader starting from the card
- The kernel with Void Linux: FriendlyElec's 6.1 on card 2, mainline 6.18 with our own T6N
  device tree on card 1
- Ethernet with the in-kernel Realtek driver
- The NVMe drive and the XFS mount
- The firewall on this kernel

Card 1 adds its own unknowns: whether FriendlyElec's bootloader accepts a mainline kernel and
device tree, whether the board keeps its usual MAC address (and so its usual IP address), and
the USB ports. HDMI shows only the boot logo on card 1, because the mainline kernel is built
without display drivers. On card 2 the screen may show a login prompt with the IP address.

## Before you start

On the current Nodo system, check there is at least 300 GB free, then shut it down:

```sh
df -h /media/monero
sudo shutdown -h now
```

## Flash the cards

Use microSD cards of 8 GB or more. Check both images first. Each checksum must match the table
above:

```sh
sha256sum out/hardware-test-1/monokulo-nanopc-t6n-mainline-sd.img
sha256sum out/hardware-test-1/monokulo-nanopc-t6n-sd.img
```

Insert one card and find its device name on your PC:

```sh
lsblk -o NAME,SIZE,TRAN,MODEL,MOUNTPOINT
```

**Warning: `dd` erases the target device completely, and this cannot be undone.** Check that the
size and model match the card, not an internal drive. Check again for each card, because the
name can change when you swap them. Replace `/dev/sdX` with the card's name.

Card 1, mainline:

```sh
sudo dd if=out/hardware-test-1/monokulo-nanopc-t6n-mainline-sd.img of=/dev/sdX bs=4M conv=fsync status=progress
sync
```

Remove it, insert card 2, run `lsblk` again, and flash the 6.1 image:

```sh
sudo dd if=out/hardware-test-1/monokulo-nanopc-t6n-sd.img of=/dev/sdX bs=4M conv=fsync status=progress
sync
```

## Boot a card

1. Insert the card into the Nodo, connect Ethernet, and power it on.
2. Wait about three minutes. The first boot grows the root partition to fill the card.
3. Find the IP address in one of these places:
   - Your router, as a device named `monokulo-nodo`. Card 1 may get a different address than
     card 2 if the board's MAC address comes out differently.
   - The Nodo's screen, card 2 only, if the console appears there.
4. Log in. Only your `id_ed25519` key works, and there is no password login:

```sh
ssh root@<device-ip>
```

The system LED blinks in a heartbeat pattern once the mainline kernel is running.

## If you can log in

Run this and send the output:

```sh
nodo-report
```

It is read-only and contains no keys or secrets. On card 1, also send the output of these, which
show the kernel features the mainline card is for and the board parts it is unsure about:

```sh
uname -r; cat /sys/kernel/security/lsm; echo
cat /proc/cmdline
ip -br link; lspci; lsblk -o NAME,SIZE,FSTYPE,PARTLABEL,MOUNTPOINT
cat /sys/class/thermal/thermal_zone*/temp
cat /sys/devices/system/cpu/cpufreq/policy*/scaling_cur_freq
dmesg | grep -iE 'error|fail|scmi|pcie|r8169|mmc|usb|fusb' | tail -n 80
```

If card 1 works, power off and test card 2 the same way too, so the two can be compared.

## If you cannot log in

Every boot saves the same report to the card twice: about 20 seconds after boot, and again two
minutes later. Power off, move the card to your PC, and read it without mounting. This works the
same for both cards. Replace `sdX` with the card's name:

```sh
sudo debugfs -R 'cat /var/log/monokulo-diag/latest.txt' /dev/sdX8
```

If that file doesn't exist, the system never got far enough to start services. Check whether the
root partition was resized, which shows that the bootloader and kernel at least ran:

```sh
sudo debugfs -R 'stat /etc/fs.resized' /dev/sdX8
```

If the Nodo started its old system with the card inserted, the board didn't boot from the card.

If card 1 shows no sign of booting (no report, no resize, no heartbeat LED), note what you saw,
then flash or insert card 2 and follow the same steps. Card 2 failing as well points at the
bootloader or the card, not at the mainline kernel.

## Going back

Power off and remove the card. The Nodo should start its original system from the eMMC. Neither
card writes to the eMMC.

## What to report

For each card you tried:

- Which card it was (mainline or 6.1)
- Whether it booted, and how you could tell
- The `nodo-report` output (and the extra card 1 commands), or the `latest.txt` file from the card
- The IP address it got
- Anything shown on the screen or by the LEDs
