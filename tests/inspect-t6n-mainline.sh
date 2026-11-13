#!/bin/sh
# Reads the mainline T6N image as plain files, with no loop devices and no root, and compares it
# with the vendor 6.1 image when one is given. Both images are only read.
#   tests/inspect-t6n-mainline.sh out/images/nanopc-t6n-mainline/monokulo-nanopc-t6n-mainline-sd.img \
#       [out/images/nanopc-t6n/monokulo-nanopc-t6n-sd.img]
# Needs sfdisk, debugfs, dtc, fdtget and python3 on the machine running it.
set -u
img=${1:?usage: inspect-t6n-mainline.sh <mainline image> [vendor image]}
ref=${2:-}
fail=0
result() { if [ "$2" = 0 ]; then echo "PASS $1"; else echo "FAIL $1"; fail=1; fi; }
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT

# "number start size name" for each partition, from the image's own GPT.
layout() {
	sfdisk -J "$1" | python3 -I -c '
import json, sys
for i, p in enumerate(json.load(sys.stdin)["partitiontable"]["partitions"], 1):
    print(i, p["start"], p["size"], p.get("name", ""))'
}
# Partition N of an image, as a file.
part() {
	where=$(layout "$1" | awk -v n="$2" '$1 == n {print $2, $3}')
	[ -n "$where" ] || { echo "no partition $2 in $1" >&2; return 1; }
	dd if="$1" of="$3" bs=512 skip="${where% *}" count="${where#* }" status=none conv=sparse
}

echo "== partitions"
layout "$img" | tee "$tmp/layout"
if [ "$(cut -d' ' -f4 "$tmp/layout" | tr '\n' ' ')" = "uboot misc dtbo resource kernel boot recovery rootfs " ]; then r=0; else r=1; fi
result "FriendlyElec's eight partitions in order" "$r"

if [ -n "$ref" ]; then
	echo "== against the vendor image $(basename "$ref")"
	layout "$ref" > "$tmp/ref-layout"
	# Everything but the root filesystem's size must match.
	if [ -s "$tmp/ref-layout" ] && [ "$(head -7 "$tmp/layout")" = "$(head -7 "$tmp/ref-layout")" ]; then r=0; else r=1; fi
	result "the boot partitions sit at the same offsets and sizes" "$r"
	if [ "$(sed -n 8p "$tmp/layout" | cut -d' ' -f2)" = "$(sed -n 8p "$tmp/ref-layout" | cut -d' ' -f2)" ]; then r=0; else r=1; fi
	result "the root filesystem starts at the same sector" "$r"
	# The loader before the GPT partitions, then uboot, misc, dtbo and the ramdisk in boot.
	cmp -s -n $((0x4000 * 512 - 0x40 * 512)) -i $((0x40 * 512)) "$img" "$ref"
	result "idbloader is byte for byte the vendor one" "$?"
	for n in 1 2 3 6 7; do
		part "$img" "$n" "$tmp/a" && part "$ref" "$n" "$tmp/b" && cmp -s "$tmp/a" "$tmp/b"
		result "partition $n ($(sed -n "${n}p" "$tmp/layout" | cut -d' ' -f4)) is byte for byte the vendor one" "$?"
	done
fi

echo "== kernel partition"
part "$img" 5 "$tmp/kernel"
python3 -I - "$tmp/kernel" "$tmp/Image" <<'PY'
import struct, sys
d = open(sys.argv[1], "rb").read()
assert d[:4] == b"KRNL", "no KRNL header"
n = struct.unpack("<I", d[4:8])[0]
body = d[8:8 + n]
crc = 0
table = []
for i in range(256):
    c = i << 24
    for _ in range(8):
        c = ((c << 1) ^ 0x04C10DB7) if c & 0x80000000 else (c << 1)
    table.append(c & 0xFFFFFFFF)
for b in body:
    crc = ((crc << 8) & 0xFFFFFFFF) ^ table[(crc >> 24) ^ b]
stored = struct.unpack("<I", d[8 + n:12 + n])[0]
assert crc == stored, f"KRNL checksum {crc:08x} != stored {stored:08x}"
assert body[0x38:0x3c] == b"ARM\x64", "not an arm64 Image"
open(sys.argv[2], "wb").write(body)
print(f"KRNL image of {n} bytes, checksum {crc:08x} matches, arm64 Image")
PY
result "kernel.img is a valid KRNL-wrapped arm64 Image" "$?"
kver=$(grep -a -o -m1 'Linux version [0-9][^ ]*' "$tmp/Image" | head -1)
echo "$kver"
echo "$kver" | grep -q 'Linux version 6\.18\.'
result "the kernel is 6.18" "$?"

echo "== resource partition"
part "$img" 4 "$tmp/resource"
python3 -I - "$tmp/resource" "$tmp" <<'PY'
import struct, sys
d = open(sys.argv[1], "rb").read()
assert d[:4] == b"RSCE", "no RSCE header"
# resource_tool.c: magic, two versions, header size, table offset and entry size in blocks,
# then the entry count, aligned to 4 bytes.
tbl_off, ent_blks, count = d[9], d[10], struct.unpack("<I", d[12:16])[0]
for i in range(count):
    e = d[(tbl_off + i * ent_blks) * 512:]
    assert e[:4] == b"ENTR"
    name = e[4:260].split(b"\0")[0].decode()
    off, size = struct.unpack("<II", e[260:268])
    print(name, size)
    open(f"{sys.argv[2]}/res-{name}", "wb").write(d[off * 512:off * 512 + size])
PY
result "resource.img parses" "$?"
cmp -s "$tmp/res-rk3588-nanopi6-rev08.dtb" "$tmp/res-rk-kernel.dtb"
result "the revision 8 and fallback device trees are the same file" "$?"
dtb=$tmp/res-rk3588-nanopi6-rev08.dtb
[ -s "$dtb" ] || { echo "FAIL no device tree to inspect"; exit 1; }
dtc -q -I dtb -O dts -o "$tmp/t6n.dts" "$dtb"
result "dtc decompiles the device tree without errors" "$?"
echo "model: $(fdtget "$dtb" / model), compatible: $(fdtget "$dtb" / compatible)"
fdtget "$dtb" / compatible | grep -q 'friendlyelec,nanopc-t6n'
result "the device tree says friendlyelec,nanopc-t6n" "$?"
# Each node the T6N needs, by its address in rk3588.dtsi.
node_ok() {
	fdtget "$dtb" "$1" compatible >/dev/null 2>&1 || return 2
	s=$(fdtget "$dtb" "$1" status 2>/dev/null || echo okay)
	[ "$s" = okay ]
}
for n in "/pcie@fe150000 pcie3x4 (NVMe)" "/pcie@fe190000 pcie2x1l2 (2.5GbE)" "/pcie@fe170000 pcie2x1l0 (M.2 E-key)" \
	"/mmc@fe2c0000 sdmmc (microSD)" "/mmc@fe2e0000 sdhci (eMMC)" "/tsadc@fec00000 tsadc (thermal)" \
	"/watchdog@feaf0000 watchdog" "/serial@feb50000 uart2 (console)" "/usb@fcd00000 USB 3.0 host"; do
	node_ok "${n%% *}"
	result "enabled: ${n#* }" "$?"
done
if node_ok /pcie@fe180000; [ "$?" = 1 ]; then r=0; else r=1; fi
result "disabled: pcie2x1l1, which the T6N does not wire" "$r"
for n in "/gpu@fb000000 gpu (Mali-G610, panthor)" "/vop@fdd90000 vop (VOP2)" "/iommu@fdd97e00 vop_mmu" \
	"/hdmi@fde80000 hdmi0" "/phy@fed60000 hdptxphy0 (HDMI PHY)" "/i2s@fddf0000 i2s5 (HDMI audio)" \
	"/hdmi0-sound hdmi0 sound card" "/i2s@fe470000 i2s0 (RT5616)" "/i2c@fec90000/codec@1b RT5616 codec" "/sound RT5616 sound card" \
	"/video-codec@fdb50000 vpu121 (hantro decoder)" "/video-codec@fdc70000 av1d (hantro AV1)" "/rga@fdb80000 rga" \
	"/ir-receiver IR receiver" "/i2c@fec80000/rtc@51 HYM8563 RTC" \
	"/dsi@fde30000 dsi1 (built-in screen)" "/phy@fedb0000 mipidcphy1 (DSI PHY)" \
	"/dsi@fde30000/panel@0 ZH055OE041 panel" "/i2c@fead0000/touchscreen@14 GT1x touchscreen"; do
	node_ok "${n%% *}"
	result "enabled: ${n#* }" "$?"
done
for n in "/hdmi@fdea0000 hdmi1" "/phy@fed70000 hdptxphy1" "/hdmi1-sound hdmi1 sound card" "/i2s@fddf4000 i2s6" "/hdmi_receiver@fdee0000 HDMI input"; do
	if node_ok "${n%% *}"; [ "$?" = 1 ]; then r=0; else r=1; fi
	result "disabled: ${n#* }, which the T6N does not have" "$r"
done
fdtget -t x "$dtb" /gpu@fb000000 mali-supply >/dev/null && fdtget -t x "$dtb" /gpu@fb000000 sram-supply >/dev/null
result "the GPU has its mali and sram supplies" "$?"
gpio4=$(fdtget -t x "$dtb" "$(fdtget "$dtb" /__symbols__ gpio4)" phandle)
if [ "$(fdtget -t x "$dtb" /hdmi@fde80000 enable-gpios)" = "$gpio4 9 0" ]; then r=0; else r=1; fi
result "hdmi0's level shifter switch is GPIO4_B1, as in FriendlyElec's T6N tree" "$r"
if [ "$(fdtget "$dtb" /dsi@fde30000/panel@0 compatible)" = zhot,zh055oe041 ]; then r=0; else r=1; fi
result "the panel is the ZHOT ZH055OE041 the original Nodo system drives" "$r"
if [ "$(fdtget "$dtb" /dsi@fde30000/panel@0 rotation)" = 270 ]; then r=0; else r=1; fi
result "the panel is turned to sit left side up in the case (rotation 270)" "$r"
vp3_ep=$(fdtget "$dtb" /__symbols__ vp3_out_dsi1 2>/dev/null)
if [ -n "$vp3_ep" ] && [ "$(fdtget "$dtb" "$vp3_ep" reg)" = 6 ]; then r=0; else r=1; fi
result "VOP2 video port 3 feeds dsi1 (ROCKCHIP_VOP2_EP_MIPI1)" "$r"
fdtget "$dtb" /chosen bootargs | grep -q 'console=tty0 console=ttyS2'
result "kernel messages go to the HDMI console and the serial port" "$?"
fdtget "$dtb" /chosen bootargs | grep -q "fbcon=font:TER16x32"
result "the screen's console uses the Terminus 16x32 font" "$?"
r=0
for a in quiet loglevel=3 vt.global_cursor_default=0 logo.nologo; do
	case " $(fdtget "$dtb" /chosen bootargs) " in *" $a "*) ;; *) echo "  bootargs lack $a"; r=1 ;; esac
done
result "the screen shows the boot splash, not boot text, a cursor or the Tux logo" "$r"
fdtget "$dtb" /pcie@fe190000/pcie@0,0/ethernet@0,0 compatible | grep -q pci10ec,8125
result "the RTL8125 node is where the ethernet0 alias points" "$?"
if [ "$(fdtget "$dtb" /aliases ethernet0)" = /pcie@fe190000/pcie@0,0/ethernet@0,0 ]; then r=0; else r=1; fi
result "ethernet0 alias, for the bootloader's MAC address" "$r"
[ "$(fdtget "$dtb" /aliases mmc0)" = /mmc@fe2c0000 ] && ! fdtget "$dtb" /aliases mmc1 >/dev/null 2>&1
result "the microSD card is mmc0, as on the vendor kernel" "$?"
fdtget "$dtb" /thermal-zones/package-thermal/trips/package-crit type | grep -q critical
result "thermal zones with a critical trip" "$?"
# The Nodo is fanless, so it throttles early (docs/THERMAL.md): 70 °C warm, 80 °C hot on the CPU
# clusters and the GPU, 100 °C critical on every sensor, the hardware reset at 110 °C.
trip() { fdtget -t i "$dtb" "/thermal-zones/$1/trips/$2" temperature 2>/dev/null; }
r=0
for z in bigcore0 bigcore2 littlecore gpu; do
	[ "$(trip "$z-thermal" "$z-alert")" = 70000 ] || { echo "  $z-alert is $(trip "$z-thermal" "$z-alert")"; r=1; }
	[ "$(trip "$z-thermal" "$z-hot")" = 80000 ] || { echo "  $z-hot is $(trip "$z-thermal" "$z-hot")"; r=1; }
	[ "$(fdtget "$dtb" "/thermal-zones/$z-thermal/cooling-maps/map1" trip 2>/dev/null)" = \
		"$(fdtget "$dtb" "/thermal-zones/$z-thermal/trips/$z-hot" phandle 2>/dev/null)" ] || { echo "  $z-hot has no cooling map"; r=1; }
done
for z in package bigcore0 bigcore2 littlecore center gpu npu; do
	[ "$(trip "$z-thermal" "$z-crit")" = 100000 ] || { echo "  $z-crit is $(trip "$z-thermal" "$z-crit")"; r=1; }
done
[ "$(fdtget -t i "$dtb" /tsadc@fec00000 rockchip,hw-tshut-temp)" = 110000 ] || { echo "  hw-tshut-temp is $(fdtget -t i "$dtb" /tsadc@fec00000 rockchip,hw-tshut-temp)"; r=1; }
result "fanless thermal limits: throttle at 70 and 80 °C, shut down at 100, reset at 110" "$r"
fdtget "$dtb" /cpus/cpu@400 operating-points-v2 >/dev/null
result "CPU frequency tables for cpufreq" "$?"
if fdtget "$dtb" / pwm-fan >/dev/null 2>&1 || fdtget -l "$dtb" / | grep -q '^pwm-fan$'; then fan=yes; else fan=no; fi
echo "pwm-fan node: $fan (the Nodo is fanless; FriendlyElec's T6N tree has none either)"
if [ "$(fdtget "$dtb" /__symbols__ chosen)" = /chosen ]; then r=0; else r=1; fi
result "the chosen label is exported for FriendlyElec's dtbo overlay" "$r"
echo "bootargs: $(fdtget "$dtb" /chosen bootargs)"
echo "bootargs_ext: $(fdtget "$dtb" /chosen bootargs_ext)"

echo "== root filesystem"
part "$img" 8 "$tmp/root.ext4"
show() { debugfs -R "$1" "$tmp/root.ext4" 2>/dev/null; }
show 'cat /etc/fstab'
show 'cat /etc/fstab' | grep -qx 'PARTLABEL=NODO_BLOCKCHAIN /media/monero xfs defaults,noatime,nofail 0 0'
result "the /media/monero line is in fstab" "$?"
show 'cat /etc/fstab' | grep -q '^/dev/mmcblk0p8 / ext4'
result "root is /dev/mmcblk0p8" "$?"
show 'ls -p /usr/lib/modules' | tr '/' '\n' | grep -q '^6\.18\.'
result "modules for 6.18 are installed" "$?"
! show 'ls -p /usr/lib/modules' | tr '/' '\n' | grep -q '^6\.1\.'
result "no vendor 6.1 modules" "$?"
kver=$(show 'ls -p /usr/lib/modules' | tr '/' '\n' | grep '^6\.18\.' | head -1)
show "cat /usr/lib/modules/$kver/modules.dep" | cut -d: -f1 > "$tmp/modules"
echo "$(wc -l < "$tmp/modules") modules in modules.dep"
r=0
for m in panthor rockchipdrm phy-rockchip-samsung-hdptx phy-rockchip-samsung-dcphy dw-hdmi-qp display-connector panel-zhot-zh055oe041 \
	snd-soc-rt5616 snd-soc-rockchip-i2s-tdm snd-soc-simple-card snd-soc-hdmi-codec snd-usb-audio \
	hantro-vpu rockchip-rga gpio-ir-recv ir-nec-decoder goodix_ts edt-ft5x06 adc-keys \
	cfg80211 mac80211 rtw88_8822ce rtw88_8822cu rtw89_8852ce mt7921e mt7925u iwlwifi iwlmvm ath11k_pci ath12k \
	bluetooth btusb hci_uart btrtl; do
	grep -q "/$m\.ko" "$tmp/modules" || { echo "  missing module: $m"; r=1; }
done
result "GPU, display, sound, codec, IR, touch, Wi-Fi and Bluetooth modules are installed" "$r"
r=0
for d in /net/can/ /net/nfc/ /infiniband/ /drivers/staging/ /fs/nfs/ /fs/btrfs/ /arch/arm64/kvm/ /drivers/xen/; do
	grep -q "$d" "$tmp/modules" && { echo "  unexpected modules under $d"; r=1; }
done
result "no CAN, NFC, InfiniBand, staging, NFS, Btrfs, KVM or Xen modules" "$r"
show 'cat /usr/share/monokulo/boot/kernel.config' > "$tmp/kconfig"
r=0
for s in CAN NFC INFINIBAND STAGING BPF_SYSCALL BPF_JIT ACPI XEN VIRTUALIZATION NFS_FS BTRFS_FS; do
	grep -qx "# CONFIG_$s is not set" "$tmp/kconfig" || { echo "  not off: CONFIG_$s"; r=1; }
done
for s in DRM_PANTHOR=m DRM_ROCKCHIP=m ROCKCHIP_VOP2=y ROCKCHIP_DW_HDMI_QP=y DRM_FBDEV_EMULATION=y FRAMEBUFFER_CONSOLE=y \
	SND_SOC_RT5616=m VIDEO_HANTRO=m CFG80211=m BT=m RTC_DRV_HYM8563=y IR_GPIO_CIR=m SECURITY_LANDLOCK=y NVME_HWMON=y; do
	grep -qx "CONFIG_$s" "$tmp/kconfig" || { echo "  not set: CONFIG_$s"; r=1; }
done
result "kernel.config: the hardware drivers are on, the excluded subsystems off" "$r"
show 'ls /usr/lib/firmware/rtl_nic' | tr -s ' \t' '\n' | grep -q '^rtl8125b-2.fw$'
result "rtl8125b-2.fw is there, uncompressed" "$?"
! show 'ls /boot' | grep -q vmlinuz
result "no QEMU kernel copy in /boot" "$?"
show 'cat /etc/sv/cuprated/conf' | grep -qx 'DATA_MOUNT=/media/monero'
result "cuprated waits for the data drive, as on the vendor card" "$?"
echo "authorized_keys sha256: $(show 'cat /root/.ssh/authorized_keys' | sha256sum | cut -c1-64)"

if [ -n "$ref" ]; then
	part "$ref" 8 "$tmp/ref.ext4"
	pkgs() { debugfs -R "cat /var/db/xbps/pkgdb-0.38.plist" "$1" 2>/dev/null | python3 -I -c '
import plistlib, sys
d = plistlib.loads(sys.stdin.buffer.read())
for k in sorted(d):
    print(d[k].get("pkgver", k))'; }
	pkgs "$tmp/root.ext4" > "$tmp/pkgs"
	pkgs "$tmp/ref.ext4" > "$tmp/ref-pkgs"
	echo "packages: $(wc -l < "$tmp/pkgs") here, $(wc -l < "$tmp/ref-pkgs") on the vendor card"
	diff "$tmp/ref-pkgs" "$tmp/pkgs" | grep '^[<>]' | sed 's/^</  vendor only:/; s/^>/  mainline only:/'
	r=1
	# Besides the kernel, only this card has Vulkan (panvk needs panthor): the loader, panvk and
	# the libraries panvk links that the vendor card lacks.
	vulkan='vulkan-loader-\|mesa-panvk-\|libdrm-\|libpciaccess-'
	if [ -s "$tmp/pkgs" ] && [ -s "$tmp/ref-pkgs" ] &&
		[ "$(diff "$tmp/ref-pkgs" "$tmp/pkgs" | grep '^<' | grep -cv 'linux-nanopc-t6n-')" = 0 ] &&
		[ "$(diff "$tmp/ref-pkgs" "$tmp/pkgs" | grep '^>' | grep -v 'linux-mainline-rk3588-' | grep -cv "$vulkan")" = 0 ] &&
		grep -q '^mesa-panvk-' "$tmp/pkgs" && grep -q '^vulkan-loader-' "$tmp/pkgs"; then r=0; fi
	result "the only package differences are the kernel and Vulkan (the loader and panvk)" "$r"
	if [ -n "$(show 'cat /root/.ssh/authorized_keys')" ] && [ "$(show 'cat /root/.ssh/authorized_keys' | sha256sum)" = "$(debugfs -R 'cat /root/.ssh/authorized_keys' "$tmp/ref.ext4" 2>/dev/null | sha256sum)" ]; then r=0; else r=1; fi
	result "the same authorized_keys as the vendor card" "$r"
fi
exit "$fail"
