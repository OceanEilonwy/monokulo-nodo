# Mainline kernel for the NanoPC-T6N

There are two T6N images. They differ only in the kernel, its device tree, its modules and the
console name:

| | `nanopc-t6n` (fallback) | `nanopc-t6n-mainline` (test first) |
|---|---|---|
| Kernel | FriendlyElec 6.1.141, `linux-nanopc-t6n` | kernel.org 6.18.62 longterm, `linux-mainline-rk3588` |
| Device tree | FriendlyElec `rk3588-nanopi6-rev08.dts` | `packages/linux-mainline-rk3588/files/rk3588-nanopc-t6n.dts` |
| Console | `ttyFIQ0` | `ttyS2` at 1500000 baud |
| Bootloader, partitions, ramdisk | FriendlyElec sd-fuse 430f0c0, plain mode | the same files, byte for byte |
| Userspace, firmware, fstab, overlay | | the same |

The reason for the second image is Landlock scoping (Linux 6.12, Landlock ABI 6), which
nodo-confine uses to stop a service from reaching other services' abstract Unix sockets or
signalling processes outside its sandbox. The vendor 6.1 kernel cannot do this, and it gets
fewer security fixes.

Unless a line says otherwise, everything here was read in the source named, not tested on a
T6N. Nothing in this document has run on real hardware yet.

## Which kernel

**6.18.62.** On 2026-11-08 kernel.org's [releases.json](https://www.kernel.org/releases.json)
lists the longterm branches as 6.18, 6.12, 6.6, 6.1, 5.15 and 5.10. 6.18 is the newest, and
6.18.62 is its latest release (2026-11-06). The stable 7.3 and the mainline 7.4-rc1 are not
longterm. The tarball's SHA-256,
`490a7aedf1b355c565d3a6c05333ccccb5da82064eca46b590448a7099d7d651`, matches kernel.org's
`sha256sums.asc`, whose signature checked out against the kernel.org checksum autosigner key
(fingerprint `B8868C80BA62A1FFFAF5FDA9632D3A06589DA6B1`, fetched over WKD).

6.18 is also the kernel Void's `linux6.18` package uses, which the image builder already boots
under QEMU to configure every image.

## Device tree support

**There is no T6N device tree in mainline.** Linux 6.18 has `rk3588-nanopc-t6.dts` (added in
6.6, commit 893c17716d0c) and `rk3588-nanopc-t6-lts.dts` (added in 6.12, commit db1dcbe5f752),
both built from `rk3588-nanopc-t6.dtsi`. Nothing in the tree or in the bindings mentions the
T6N. The commit-to-release mapping was checked with GitHub's compare API on the torvalds/linux
mirror.

The T6N is a T6 variant, so our tree, `rk3588-nanopc-t6n.dts`, includes mainline's T6 dtsi
and applies what differs in FriendlyElec's T6N tree (`rk3588-nanopi6-rev08.dts` at
kernel-rockchip commit 30d3e1c, the same source as the 6.1 kernel). This is the comparison:

| Area | Mainline T6 dtsi | FriendlyElec T6N (rev08) | Our T6N tree |
|---|---|---|---|
| PCIe 3.0 x4, NVMe | `pcie3x4`, reset GPIO4_B6, power GPIO2_C5 | same | unchanged |
| 2.5GbE, RTL8125 | `pcie2x1l2`, reset GPIO4_A4 | same, plus a child node for the NIC with `ethernet0` alias | adds the NIC node and alias, so the bootloader can write the MAC address |
| Second 2.5GbE | `pcie2x1l1`, reset GPIO4_A2, power GPIO4_C2 | disabled; those pins go to `pcie2x1l0` | disabled |
| M.2 E-key (Wi-Fi) | `pcie2x1l0`, reset GPIO4_B3 | reset GPIO4_A2, power GPIO4_C2 | as FriendlyElec. No Wi-Fi driver is built |
| microSD | `sdmmc`, alias `mmc1` | `sdmmc`, alias `mmc0` | alias `mmc0`, so the card is `mmcblk0` on both kernels |
| eMMC | `sdhci`, alias `mmc0` | `sdhci`, alias `mmc2` | alias `mmc2` |
| Card power | GPIO4_A5 active low, I/O voltage from RK806 PLDO5 | same | unchanged |
| PMIC, CPU regulators | RK806 on SPI2, RK8602/8603 on I2C0 | same | unchanged |
| RTC | HYM8563 on I2C6 at 0x51 | same | unchanged |
| Watchdog | `wdt`, enabled in rk3588-base.dtsi | enabled | unchanged |
| Fan | `pwm1` enabled, no `pwm-fan` node | no fan node, `pwm1` off | `pwm1` off, no fan, as FriendlyElec |
| Thermal | `tsadc` on, rk3588 thermal zones with CPU and GPU passive cooling and critical trips | similar | unchanged |
| CPU frequency | rk3588-opp.dtsi OPP tables (since 6.11), SCMI clocks from BL31 | vendor tables | unchanged |
| Type-C port 1 | FUSB302 on I2C6, `usb_host0_xhci` | same | unchanged |
| Type-C port 2 | none; GPIO4_B0 powers the T6's USB-A port permanently | FUSB302 on I2C4 (int GPIO4_A3), GPIO4_B0 is this port's VBUS | as FriendlyElec, so VBUS is only switched on after the port negotiates the source role |
| USB-A 3.0 | `usb_host1_xhci` on USBDP PHY1 | `usbhost3_0` on combphy2, power GPIO3_A5 | `usb_host2_xhci` on combphy2, power GPIO3_A5 |
| User LED, SPI flash | present | absent | removed |
| Console | `serial2:1500000n8` | FIQ debugger on UART2 | UART2 as `ttyS2` |
| Display, audio, camera, HDMI in | enabled | enabled | left in the tree; the drivers are not built |

### Per subsystem

- **Ethernet.** The RTL8125 (rev 05, an RTL8125B) on `0004:41:00.0` is driven by the in-tree
  `r8169` driver, built in. It asks for `rtl_nic/rtl8125b-2.fw` when the link comes up, after
  the root filesystem is mounted. That file comes from Void's `linux-firmware-network`, the same
  package and filter as the 6.1 image. r8169 works without it, with a warning. The MAC address:
  FriendlyElec's bootloader contains `fdt_fixup_ethernet`-style code (`ethaddr`,
  `local-mac-address` strings) and FriendlyElec's tree gives it an `ethernet0` alias for the
  NIC, so ours does the same. **Unverified:** that the bootloader fills it in for our tree. If
  it does not, r8169 uses the chip's own address or a random one, and the router may hand out a
  different IP address than before.
- **PCIe.** `pcie-dw-rockchip`, the Naneng combo PHY and the Synopsys PCIe 3.0 PHY are built in.
  Mainline numbers PCIe domains the same way as the vendor kernel (`linux,pci-domain` in
  rk3588.dtsi), so the NIC should again be `0004:41:00.0` and the NVMe drive `0000:01:00.0`.
- **NVMe.** Built in. The drive's XFS partition is mounted by the same fstab line,
  `PARTLABEL=NODO_BLOCKCHAIN /media/monero xfs defaults,noatime,nofail`. XFS is built in.
- **microSD and eMMC.** `dw_mmc-rockchip` and `sdhci-of-dwcmshc`, built in, with the SPI, RK806
  regulator, pin control and GPIO drivers they depend on. They must be built in: FriendlyElec's
  ramdisk, which mounts the root filesystem, contains no kernel modules.
- **USB.** DWC3, the USB2 and USBDP PHYs, EHCI and OHCI are built in, FUSB302 is a module.
  `usb-storage` and `uas` are modules.
- **Thermal and fan.** `rockchip_thermal` and CPU cooling are built in, `pwm-fan` is a module
  with nothing to bind to. **Unverified:** whether the Nodo case has a fan. If it does, neither
  kernel drives it from the device tree.
- **cpufreq.** `cpufreq-dt` with SCMI clocks. **Unverified:** that FriendlyElec's BL31 answers
  the SCMI requests mainline makes. Mainline rk3588 trees are written for Rockchip's rkbin BL31,
  which is what FriendlyElec ships (the running system reports `bl31-v1.46`).
- **Watchdog.** `dw_wdt`, built in. Nothing opens it, which is the same as on the 6.1 image.
- **RTC.** `rtc-hym8563`, built in.
- **Console.** On mainline the debug UART is `ttyS2`, 1500000 8N1, where the vendor kernel has
  the FIQ debugger's `ttyFIQ0`. Void has no `agetty-ttyS2` or `agetty-ttyFIQ0` service, so
  neither image starts a login prompt on it, and root's password is locked on both anyway. The
  serial port only shows kernel messages. HDMI shows the bootloader's logo and nothing after.

## Bootloader

**Decision: keep FriendlyElec's bootloader, unchanged.** The mainline image runs the vendor
board's `assemble.sh` itself, so the idbloader, `uboot.img`, `misc.img`, `dtbo-plain.img`, the
ramdisk `boot.img` and the partition table are the same files at the same offsets.
`tests/inspect-t6n-mainline.sh` checks this byte for byte against the 6.1 image.

Why this works, from reading the pinned files (sd-fuse_rk3588 commit
430f0c09ff7ce75f25eb21185c28016c27d660c1):

- `uboot.img` is a FIT with U-Boot 2017.09 (`gc5c053fa557-250917`) and Rockchip's BL31 and
  BL32. Its environment has `bootcmd=boot_fit;boot_android ${devtype} ${devnum};bootrkp;run distro_bootcmd;`.
  On this card `bootrkp` boots: it reads the `kernel` partition (a `KRNL`-wrapped arm64 Image),
  the `resource` partition (an `RSCE` container of device trees and logos) and the `boot`
  partition as the initramfs.
- It picks the device tree from `resource` by the name `rk3588-nanopi6-rev%02x.dtb`, using the
  board revision it detects, and falls back to `rk-kernel.dtb`. The T6N is revision 08.
- It builds the command line from the device tree's `/chosen/bootargs` and
  `/chosen/bootargs_ext`, after applying an overlay from the `dtbo` partition. The overlay in
  `dtbo-plain.img` sets `bootargs_ext` to `... root=/dev/mmcblk0p8 rw rootfstype=auto` and finds
  its target through the `chosen` label in `__symbols__`.
- The ramdisk is an Ubuntu initramfs-tools image with udev and no kernel modules, so it does not
  care which kernel runs it, as long as the drivers for the card are built in.

So the kernel package produces the same two files the vendor package does: `kernel.img`, made
with FriendlyElec's `mkkrnlimg`, and `resource.img`, made with their `resource_tool`, both
compiled from the pinned kernel-rockchip commit (their sources, and FriendlyElec's logo, are
distfiles with SHA-256 checksums in the template). `resource.img` holds our tree twice, as
`rk3588-nanopi6-rev08.dtb` and `rk-kernel.dtb`, plus the logos. The tree is compiled with
`dtc -@` and labels `/chosen`, so the vendor overlay applies to it as it does to theirs. Applying
`dtbo-plain.img` with `fdtoverlay` gives `root=/dev/mmcblk0p8`. Our tree also carries that root
in its own `bootargs_ext`, in case the overlay is not applied. The resulting command line is

    earlycon=uart8250,mmio32,0xfeb50000 console=ttyS2,1500000n8 coherent_pool=1m
    cgroup_enable=memory cgroup_memory=1 consoleblank=0 root=/dev/mmcblk0p8 rw rootfstype=auto

plus whatever the bootloader adds itself (`storagemedia=`, `androidboot.*`), as on the 6.1 image.

The alternative was mainline U-Boot, which has `nanopc-t6-rk3588_defconfig` since v2024.01
(commit b0b8086898f8) and is what Armbian uses for its mainline T6 kernels. It would need
Rockchip's rkbin DDR init and BL31 (or TF-A), pinned, and an extlinux or EFI boot on a new
partition layout. That changes the boot chain, the layout and the DDR init all at once, and it
has no T6N support either, so a failure could not be blamed on the kernel. Keeping the vendor
bootloader makes the kernel the only variable, at the cost of depending on vendor U-Boot fixups
behaving with a mainline tree.

**Unverified, and the main boot risks:**

- That vendor U-Boot's fixups (memory banks, MAC address, overlay, its board-specific code) do
  nothing harmful to a mainline tree. People do boot mainline kernels with Rockchip's 2017.09
  U-Boot on other RK3588 boards, but this exact build with this tree has not been tried.
- That it detects revision 08. If not, it should load `rk-kernel.dtb`, which is the same tree.
- That Rockchip's BL31 SCMI clock and power-domain interface matches what 6.18 expects.

## Kernel configuration

arm64 `defconfig` plus `packages/linux-mainline-rk3588/files/monokulo.config`. The build fails
if `olddefconfig` drops or changes any line of the fragment. In short:

- Only `ARCH_ROCKCHIP` among SoC families. No ACPI, KVM, DRM, sound, media, Wi-Fi, Bluetooth,
  CAN, NFC, staging, NFS or Btrfs. The Image is about 22 MB, inside the 40 MiB kernel partition.
- `CONFIG_LSM="landlock,lockdown,yama"` with Landlock, seccomp filters, and user, PID, network,
  IPC and UTS namespaces. `BPF_SYSCALL` is off: nothing on the appliance loads eBPF, and seccomp
  uses classic BPF.
- nftables and the matches packages use, conntrack, NAT, veth, macvlan, tun.
- dm-crypt with the user-space crypto API for LUKS2, loop, ext4 and XFS built in, vfat, exfat,
  overlayfs and FUSE as modules, NVMe built in, USB storage as modules.
- The T6N drivers above, built in where the boot path needs them.
- virtio PCI and MMIO, virtio block, net and console, PL011 and PL031, so the same kernel boots
  QEMU's `virt` machine.
- `/proc/config.gz`, zstd-compressed modules and zstd firmware loading.

## What was tested without hardware

`boards/qemu-virt-mainline` boots this kernel package under QEMU with the T6N package set, and
runs the usual boot checks (`tests/qemu-boot-test.sh qemu-virt-mainline`) plus
`tests/kernel-checks.sh`. The image builder also boots this kernel once to configure that image.
The kernel checks:

- `/sys/kernel/security/lsm` lists landlock, and `nodo-confine --check` reports
  `landlock_abi=7 abstract_scope=yes` on 6.18.
- nodo-confine's own integration tests (`crates/nodo-confine/tests/confine.rs`, cross-built as
  static aarch64 binaries by `tests/build-landlock-probe.sh`) pass inside the guest, and the
  check fails if they skip the scoping assertions. They show that a confined process cannot
  connect to an abstract socket made outside its sandbox, that its own abstract socket and a
  granted socket file still work, and that without a Unix grant it cannot make a Unix socket.
- A confined shell can signal itself but not PID 1, which root outside can.
- LUKS2 on a loop device, opened, formatted XFS, mounted and written. An overlayfs mount.

QEMU proves that the kernel boots, that the userspace runs on it, that Landlock ABI 6
scoping, seccomp, namespaces, nftables, LUKS2, XFS and overlayfs work, and that the image
builds. It cannot prove anything specific to the board: the vendor bootloader loading
`kernel.img` and `resource.img`, the device tree, the RK806 and the card, PCIe link training,
the RTL8125, the NVMe drive, USB, thermal, cpufreq or the MAC address.

`tests/inspect-t6n-mainline.sh` reads the finished card image as plain files and checks the
partition layout, that the bootloader files match the 6.1 image byte for byte, the `KRNL`
header and checksum, the `RSCE` contents, that the device tree decompiles cleanly with the
needed nodes enabled, and that the root filesystem and package list match the 6.1 image except
for the kernel.

## Sources

- kernel.org releases: <https://www.kernel.org/releases.json>, tarball and `sha256sums.asc`
  under <https://cdn.kernel.org/pub/linux/kernel/v6.x/>
- Mainline device trees: `arch/arm64/boot/dts/rockchip/rk3588-nanopc-t6.dtsi`, `rk3588-base.dtsi`,
  `rk3588-extra.dtsi` and `rk3588-opp.dtsi` in linux-6.18.62; history from the torvalds/linux
  mirror on GitHub (commits 893c17716d0c, db1dcbe5f752, 276856db91b4)
- FriendlyElec's T6N tree: `rk3588-nanopi6-rev08.dts` and `rk3588-nanopi6-common.dtsi` at
  <https://github.com/friendlyarm/kernel-rockchip/tree/30d3e1c2470a7282f37b96fdd4354314dda69aa3>
- FriendlyElec's bootloader files: `prebuilt/` at
  <https://github.com/friendlyarm/sd-fuse_rk3588/tree/430f0c09ff7ce75f25eb21185c28016c27d660c1>,
  read with `dtc`, `strings` and the decoders in `tests/inspect-t6n-mainline.sh`
- Mainline U-Boot: `configs/nanopc-t6-rk3588_defconfig` in <https://github.com/u-boot/u-boot>
  (commit b0b8086898f8, in v2024.01)
- Armbian's T6 board file, `config/boards/nanopct6.eos` in <https://github.com/armbian/build>:
  mainline U-Boot for mainline kernels, no T6N board
- The running T6N's model, compatible string, command line, `lspci` and `lsmod`, from the owner
