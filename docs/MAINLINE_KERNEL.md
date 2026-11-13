# Mainline kernel for the NanoPC-T6N

There are two T6N images. They differ only in the kernel, its device tree, its modules, the
console name and Vulkan, which needs the mainline kernel's GPU driver:

| | `nanopc-t6n` (fallback) | `nanopc-t6n-mainline` (test first) |
|---|---|---|
| Kernel | FriendlyElec 6.1.141, `linux-nanopc-t6n` | kernel.org 6.18.62 longterm, `linux-mainline-rk3588` |
| Device tree | FriendlyElec `rk3588-nanopi6-rev08.dts` | `packages/linux-mainline-rk3588/files/rk3588-nanopc-t6n.dts` |
| Console | `ttyFIQ0` | `ttyS2` at 1500000 baud |
| Bootloader, partitions, ramdisk | FriendlyElec sd-fuse 430f0c0, plain mode | the same files, byte for byte |
| Vulkan | none (kbase cannot run panvk) | `vulkan-loader` and `mesa-panvk` |
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
| M.2 E-key (Wi-Fi) | `pcie2x1l0`, reset GPIO4_B3 | reset GPIO4_A2, power GPIO4_C2 | as FriendlyElec, with Wi-Fi and Bluetooth drivers for common cards |
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
| GPU | Mali-G610 `gpu` with `mali-supply` | the same, plus `mem-supply` (the same rail) | enabled, with `sram-supply` for that rail |
| HDMI out | `hdmi0` and `hdmi1`, both HDPTX PHYs, both I2S audio links | `hdmi0` only, with `enable-gpios` GPIO4_B1 (T6: another pin) | `hdmi0` with GPIO4_B1; `hdmi1`, `hdptxphy1`, `i2s6` and its sound card off |
| HDMI in | `hdmi_receiver` with a 160 MiB CMA pool | absent | off, with its pool |
| Headphone jack | RT5616 on I2C7, `i2s0`, jack detect GPIO1_C4 | the same | unchanged |
| IR receiver | `gpio-ir-receiver` on GPIO0_D4 | the same pin, as a PWM remote input | unchanged |
| DisplayPort over Type-C | none | `dp0`/`dp1` alt mode on both ports | not wired up (driver built, nodes off) |
| MIPI-DSI, MIPI-CSI | connectors only | DSI1 with the 5.5" ZH055OE041 AMOLED on video port 3, GT1x touch on I2C5, CSI0/CSI1 | DSI1, its DC-PHY, the panel and the touchscreen as FriendlyElec; DSI0 and the cameras off |

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
- **RTC.** `rtc-hym8563`, built in, so the kernel sets the clock from it at boot
  (`RTC_HCTOSYS`). Both images also run `hwclock --hctosys` early in boot
  (`/etc/runit/core-services/05-nodo-rtc.sh`, for a driver that was late) and set
  `HARDWARECLOCK="UTC"` in `/etc/rc.conf`, so Void's shutdown writes the clock back, and chrony's
  `rtcsync` keeps it right while the node runs. The time survives a reboot without network.
- **Console.** On mainline the debug UART is `ttyS2`, 1500000 8N1, where the vendor kernel has
  the FIQ debugger's `ttyFIQ0`. Void has no `agetty-ttyS2` or `agetty-ttyFIQ0` service, so
  neither image starts a login prompt on it. The serial port shows kernel messages.
- **The built-in screen.** The Nodo's screen is not on HDMI: it is a ZHOT ZH055OE041 5.5"
  1080x1920 AMOLED panel on `dsi1`, fed by VOP2 video port 3, the same as in the device tree the
  original Nodo system boots (read from its eMMC on 2026-11-12, where `card0-DSI-1` is connected and
  `card0-HDMI-A-1` is not). Mainline has no driver for this panel; `files/panel-zhot-zh055oe041.c`
  carries FriendlyElec's (their `panel-raydium-rm67191.c` at kernel-rockchip 30d3e1c: the panel's
  command table, power-on sequence and timing, 133.711 MHz for 1080x1920 at 60 Hz) for mainline's
  panel API, and the template adds it to the kernel tree. The panel has no backlight; its DCS
  brightness is offered as one. The touchscreen is a Goodix GT1x on I2C5, which mainline's `goodix`
  driver handles as `goodix,gt1151`. The DC-PHY, panel and touch drivers are modules, so the screen
  lights once udev has loaded them. Verified on the Nodo on 2026-11-13: the panel shows a steady
  picture and touch registers (Goodix ID 1158). Three things were needed beyond FriendlyElec's
  tree, found by dumping the DSI host, DC-PHY and VOP2 registers on both systems and comparing:
  - **Pixel clock 132 MHz.** VOP2's DSI video port only makes 132 MHz here (GPLL / 9); with the
    mode at FriendlyElec's 133.711 MHz, mainline's DSI host timed lines for the wrong rate.
  - **VOP2 pre-scan hblank of at least 8.** Mainline writes the mode's hsync length into
    `VP_PRE_SCAN_HTIMING`; Rockchip's BSP clamps it to 8, since a smaller value lets the window
    reset corrupt the line start. The panel's hsync is 4 pixels, and without the clamp the picture
    jittered along the line. `patches/0001-drm-rockchip-vop2-pre-scan-hblank-minimum.patch`.
  - **High-speed blanking in the back porch** (`MIPI_DSI_MODE_VIDEO_NO_HBP`, the DSI host's
    `BLK_HBP_HS_EN`), as the original system's DSI host has it.
  The panel sits in the case left side up (`rotation = <270>`); the driver reports the
  orientation and fbcon rotation is built in, so the console reads the right way up, in Terminus
  16x32 (`fbcon=font:TER16x32`, 120 by 33 characters). The kernel boots with `quiet loglevel=3
  vt.global_cursor_default=0 logo.nologo`, so the screen stays black, without boot text, a cursor
  or the Tux logo, until the boot splash (docs/SCREEN.md); errors still reach the consoles.
- **HDMI and the text console.** VOP2, `dw-hdmi-qp` and the HDPTX PHY drive `hdmi0`, and DRM's
  fbdev emulation puts a text console on it once `rockchipdrm` loads (udev loads it from the root
  filesystem, so the screen shows the boot logo until then). `console=tty0` sends kernel messages
  there too. Void starts `agetty` on tty1 to tty6, so with a USB keyboard the screen shows the
  login prompt with the address from `/etc/issue` (`ssh root@<address>`). Logging in there is not
  possible: root's password stays locked, as on SSH, and no default password is added. The
  prompt is for reading the address; the owner manages the node over SSH and the TUI.
- **GPU.** `panthor` for the Mali-G610, with `arm/mali/arch10.8/mali_csffw.bin` from
  linux-firmware. eudev's rules give `/dev/dri/card0` to the `video` group (mode 0660). They would
  make the render node `/dev/dri/renderD128` open to every user (0666); `60-nodo-gpu.rules` in the
  board overlay gives render nodes to the `gpu` group instead (root:gpu 0660). nodo-tools creates
  that group and no account is a member, so no service reaches the GPU driver unless the owner
  approved the GPU for its package (see "Vulkan" below). Void has no `render` group.
  **Unverified:** that panthor probes with FriendlyElec's BL31. It needs the GPU's SCMI clock and
  power domain, the same interface cpufreq depends on. The vendor 6.1 kernel's Mali driver is
  Arm's kbase (`/dev/mali0`), which Mesa cannot use and which asks for FriendlyElec's own
  firmware, not linux-firmware's, so the vendor card has no Vulkan; the same rule is on it but
  matches nothing there.
- **Vulkan (mainline card only).** Void's `vulkan-loader` (0.6 MiB, needs only glibc) and
  `mesa-panvk`, our headless Mesa with only panvk, the Vulkan driver for Mali (see below). Void's
  own Mesa would bring libllvm22 (135 MiB), libz3 (30 MiB), mesa-libgallium (39 MiB), mesa
  (17 MiB) and X11 libraries, about 235 MiB, and even its `mesa-vulkan-panfrost` alone links
  X11, XCB and Wayland. `nodo-gpu-check` (nodo-tools) is the on-board probe, and nodo-report
  runs it.

## Vulkan: panvk

**Mesa 26.2.5** (`packages/mesa-panvk`), from archive.mesa3d.org. On 2026-11-09 the newest
stable releases were 26.2.6 (2026-10-28, under two weeks old) and 26.2.5 (2026-10-14); 26.3 is
still at rc3. The tarball's SHA-256,
`794b3d1729075dae7ebd47c7283b7ca0cdb5dec668f7f11cc0a5d82f6138a0a5`, matches the release notes
(`docs/relnotes/26.2.5.rst`), and its signature checked out against Eric Engestrom's key
`57551DE15B968F6341C248F68D8E31AFC32428A6` (fetched from keys.openpgp.org).

What Mesa 26.2.5 says about panvk on this GPU, from its own docs and source:

- `docs/drivers/panfrost.rst`: G310 and G610 (Valhall, v10) get **Vulkan 1.4**, and PanVK is
  **conformant on Mali-G610** (Khronos submission 939), non-conformant elsewhere.
- `docs/features.txt`: Vulkan 1.1, 1.2, 1.3 and 1.4 are all done for `panvk/v10+`; among the
  extensions compute uses, `VK_KHR_8bit_storage`, `VK_KHR_shader_float16_int8`,
  `VK_KHR_buffer_device_address`, `VK_KHR_shader_integer_dot_product`,
  `VK_KHR_shader_subgroup_rotate`, `VK_KHR_shader_subgroup_extended_types` (v10+) and
  `VK_EXT_shader_atomic_float` are done; `VK_KHR_cooperative_matrix` is not.
- `panvk_vX_physical_device.c`: API version 1.4 for v10 and later (1.0 before), an
  integrated GPU, `maxStorageBufferRange` = UINT32_MAX, `maxPushConstantsSize` = 256,
  `maxPerStageDescriptorStorageBuffers` = 64, `shaderInt64` on, and
  `maxComputeWorkGroupInvocations` and `maxComputeWorkGroupSize` from the GPU's own
  `thread_max_workgroup_size`, which panthor reads from the hardware (1024 on Valhall parts
  generally; the board will show it).
- `panvk_physical_device.c`: one device-local heap and memory types DEVICE_LOCAL alone,
  DEVICE_LOCAL | HOST_VISIBLE | HOST_COHERENT (always), plus a cached coherent type when the GPU
  is I/O-coherent and a cached non-coherent type when the kernel offers write-back mappings.

So the Monokulo wallet scanner's needs (monokulo-8ar: a 1.0 instance, 32-bit integer compute,
`maxStorageBufferRange` of at least 39,321,600 bytes, 64 invocations per workgroup, 20 bytes of
push constants, 4 storage buffers per stage, DEVICE_LOCAL and HOST_VISIBLE | HOST_COHERENT
memory) are all within what panvk reports. nodo-gpu-check prints each of those limits with a
PASS or FAIL against the scanner's threshold.

The build has only `-Dvulkan-drivers=panfrost`, no Gallium driver, `-Dplatforms=` (headless and
`VK_KHR_display` only, no X11 or Wayland), no OpenGL, GLES, EGL, GLX, GBM or glvnd, no video
codecs, no layers or tools, no xmlconfig (so no expat), `-Dllvm=disabled`, a release build,
stripped. panvk's built-in shaders are OpenCL C that Mesa compiles while building with
`mesa_clc` (LLVM 22, clang and SPIRV-LLVM-Translator) and `panfrost_compile`; those must run on
the build machine, so the recipe first configures a native build of just those tools, then
cross-builds the driver with them as system tools (`-Dmesa-clc=system
-Dprecomp-compiler=system`). LLVM is therefore a build dependency only and never reaches the
card. The package installs `libvulkan_panfrost.so` (15.2 MiB, all of panvk's per-architecture
code and compilers) and `panfrost_icd.aarch64.json`, 15.2 MiB in all. It links libdrm, zlib,
libzstd (shader cache compression), libudev (found in the build root; eudev is on the card
anyway) and libstdc++, all already on the card or small.

### The GPU permission

GPU access is a package permission the owner approves, like internet access. A manifest asks
with

```toml
[devices]
gpu = true
```

which shows in the install and update review as "uses the GPU for computation (Vulkan)", marked
as hardware access. An update that starts asking for it is a new permission, so it never
installs unattended and waits for the owner's approval like any other. When approved, nodo-gen's
run script:

- runs the service with `gpu` as a supplementary group (`chpst -u _app:_app:gpu`); chpst sets
  exactly the groups listed, so without the grant the service has no `gpu` group, and no
  account is ever added to the group in `/etc/group`;
- binds each `/dev/dri/renderD*` character device into the sandbox with `--dev-bind`, after
  `--dev /dev`, and never a card node, which can drive displays;
- binds, read-only, what the Vulkan loader and Mesa read through libdrm to identify the node:
  the GPU's device directory in sysfs (for its `uevent`), a `/sys/dev/char/<major>:<minor>`
  link to it, and an empty directory for its bus, which libdrm resolves the `subsystem` link
  to. Nothing else of `/sys` appears.

A service without the grant sees no `/dev/dri` and no `/sys` at all. The ICD manifest is in
`/usr/share/vulkan/icd.d`, which every sandbox already has read-only with `/usr`. Mesa's shader
cache goes to `$HOME/.cache`, and `HOME` is the service's own writable data directory. The rest
of the sandbox is unchanged: nodo-confine's seccomp filter refuses only io_uring, foreign ABIs
and (without a Unix grant) Unix sockets, so DRM ioctls, mmap of buffer objects and fence waits
pass, and its Landlock domain only scopes abstract sockets and signals, so it does not restrict
device ioctls. Under the user namespace bwrap creates, the group shows as `nobody` inside the
sandbox, but the kernel checks the real group on open.

Tests: `crates/nodo-gen/tests/generated.rs` runs the generated script against a fake `/dev/dri`
and `/sys` (render and card nodes, a platform device) and checks exactly what is bound;
`tests/gpu-sandbox.sh` (VM tier) installs a granted and an ungranted app through the helper on
the x86 VM with vgem's render node, or a stand-in device node when the kernel lacks vgem, and
checks the group, what each sandbox sees and a DRM ioctl from inside the granted one;
`tests/kernel-checks.sh` shows that under QEMU the loader loads panvk, which finds no Mali.
What only the board can show: that panthor probes, that panvk finds the G610 through the
sandbox's sysfs subset, and that the compute shader gives the right answer (nodo-gpu-check).
- **Sound.** HDMI audio through `i2s5` and `dw-hdmi-qp`'s hdmi-codec, and the RT5616 headphone
  jack and microphone on `i2s0`, both through `simple-audio-card`; USB audio for adapters.
  `nodo-sound` restores the mixer state at boot and saves it at shutdown (`alsactl`); before
  the first save it turns on the RT5616's DAC-to-headphone path at about 70 %. HDMI audio has no
  mixer controls. Void's own `alsa` service is not used, because it restarts every second
  while there is no saved state or no card.
- **Video codecs.** `hantro` for the VPU121 decoder (H.264, MPEG-2, VP8), the four VEPU121 JPEG
  encoders and the AV1 decoder, and `rockchip-rga` for the 2D engine. The RKVDEC2 H.265/VP9
  decoder has no driver in 6.18. No camera: 6.18 has no RK3588 ISP or VICAP driver.
- **IR receiver.** `gpio-ir-recv` with the common protocol decoders; `ir-keytable` (v4l-utils)
  loads keymaps.
- **MIPI-DSI.** The DSI2 host and the Samsung DC-PHY are built, with Goodix and FocalTech touch
  drivers. `dsi1` drives the built-in screen (above); `dsi0` stays off.
- **Wi-Fi and Bluetooth.** Drivers for M.2 (PCIe) and USB cards: Realtek rtw88/rtw89, MediaTek
  MT7921/MT7922/MT7925, Intel AX200/AX210/BE200 and Qualcomm WCN6855/WCN7850, with Bluetooth
  over USB (`btusb`) and UART (`hci_uart`), and the matching firmware. wpa_supplicant, bluez and
  dbus are installed but not started, and a udev rule soft-blocks every Wi-Fi and Bluetooth radio
  when it appears, so both are off until the owner turns them on (`rfkill unblock`, then the
  service). A udev rule keeps the kernel's interface names: the wired port is `eth0`, the first
  Wi-Fi card `wlan0`, Bluetooth `hci0`. dhcpcd manages every interface, so `wlan0` gets an
  address once it is associated, with a higher route metric than `eth0`, and the firewall has no
  per-interface rules, so `wlan0` is treated exactly like the wired LAN.
- **Devices.** eudev's `udevd` runs from early boot (Void's core service) and is then supervised
  as a service. It loads modules by modalias at boot and on hotplug, loads firmware, and keeps
  the hardware database (`/etc/udev/hwdb.bin`).

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

- Only `ARCH_ROCKCHIP` among SoC families. No ACPI, KVM, Xen, CAN, NFC, InfiniBand, staging,
  NFS or Btrfs. The RK3588's CAN controllers have no transceiver or connector on the T6N
  (FriendlyElec's tree enables none), and the video decoder 6.18 has for it (hantro) is not in
  staging. `kernel.img` is 21.8 MiB (22,876,684 bytes), inside the 40 MiB kernel partition and
  64 KiB more than before the hardware drivers below were added, since they are modules.
- Display, GPU, sound, video codecs, IR, touch, Wi-Fi and Bluetooth as modules, as listed
  above. defconfig's drivers for other SoCs' display, sound and Wi-Fi blocks are turned off.
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
for the kernel and Vulkan. `tests/inspect-t6n-platform.sh` checks the GPU's udev rule and group
on both cards, and the ICD manifest, `libvulkan_panfrost.so` and the loader on the mainline card
and their absence on the vendor card.

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
