#!/bin/sh
# Reads a finished T6N card image (either kernel) as plain files, with no loop devices and no
# root, and checks what the image adds on top of the boot path: the platform packages and the
# services started at boot, nodo-localnet's dhcpcd block, restricted RPC on loopback, cuprated
# under nodo-confine, and nodo-report's abstract socket list. The image is only read.
#   tests/inspect-t6n-platform.sh out/images/nanopc-t6n/monokulo-nanopc-t6n-sd.img
# Needs sfdisk, debugfs and python3 (3.11 or later) on the machine running it.
set -u
img=${1:?usage: inspect-t6n-platform.sh <T6N image>}
fail=0
result() { if [ "$2" = 0 ]; then echo "PASS $1"; else echo "FAIL $1"; fail=1; fi; }
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT

# Partition 8 is the root filesystem in FriendlyElec's layout.
where=$(sfdisk -J "$img" | python3 -I -c '
import json, sys
p = json.load(sys.stdin)["partitiontable"]["partitions"][7]
print(p["start"], p["size"])')
dd if="$img" of="$tmp/root.ext4" bs=512 skip="${where% *}" count="${where#* }" status=none conv=sparse
show() { debugfs -R "$1" "$tmp/root.ext4" 2>/dev/null; }

debugfs -R "cat /var/db/xbps/pkgdb-0.38.plist" "$tmp/root.ext4" 2>/dev/null | python3 -I -c '
import plistlib, sys
for k, v in sorted(plistlib.loads(sys.stdin.buffer.read()).items()):
    print(k, v.get("pkgver", k))' > "$tmp/pkgs"
echo "== $(wc -l < "$tmp/pkgs") packages; the platform:"
grep -E '^(monokulo-base|cuprated|cuprated-service|nodo-tools|nodo-caddy|nodo-ddns|nodo-localnet|linux-nanopc-t6n|linux-mainline-rk3588) ' "$tmp/pkgs" | sed 's/^/  /'
for p in nodo-tools nodo-caddy nodo-ddns nodo-localnet cuprated-service monokulo-base; do
	grep -q "^$p " "$tmp/pkgs"
	result "$p is installed" "$?"
done
! grep -q '^monokulo ' "$tmp/pkgs"
result "Monokulo is not preinstalled; it comes through the helper from a feed" "$?"

# debugfs's parseable listing is /inode/mode/uid/gid/name/size/ per entry.
links=$(show 'ls -p /etc/runit/runsvdir/default' | cut -d/ -f6 | grep -v '^\.\.\?$' | grep . | sort | tr '\n' ' ')
echo "== started at boot: $links"
r=0
for s in sshd dhcpcd chronyd nftables cuprated monokulo-diag nodo-helper nodo-healthd nodo-autoupdate; do
	case " $links " in *" $s "*) ;; *) echo "  not started: $s"; r=1 ;; esac
	show "stat /etc/sv/$s/run" | grep -q 'Type: regular' || { echo "  no run script: $s"; r=1; }
done
result "the base services, the helper, the health daemon and the update timer start at boot" "$r"
r=0
for s in nodo-caddy nodo-ddns nodo-localnet nodo-mdns; do
	case " $links " in *" $s "*) echo "  started at boot: $s"; r=1 ;; esac
	show "stat /etc/sv/$s/run" | grep -q 'Type: regular' || { echo "  no run script: $s"; r=1; }
done
result "the web services are installed but left for the helper to start" "$r"
show 'stat /var/lib/nodo' | grep -q 'Type: directory' && show 'stat /usr/bin/nodo-tui' | grep -q 'Type: regular'
result "the helper's state directory and the TUI are there" "$?"

echo "== dhcpcd"
show 'cat /etc/dhcpcd.conf' > "$tmp/dhcpcd.conf"
sed -n '/^interface nodo-local$/,$p' "$tmp/dhcpcd.conf" | sed 's/^/  /'
r=0
for line in ipv4only noipv4ll nogateway 'metric 4000' 'nohook resolv.conf'; do
	sed -n '/^interface nodo-local$/,/^interface /p' "$tmp/dhcpcd.conf" | grep -qx "	$line" || { echo "  missing: $line"; r=1; }
done
result "dhcpcd.conf has the nodo-local block" "$r"
grep -qx 'duid' "$tmp/dhcpcd.conf" && grep -qx 'controlgroup wheel' "$tmp/dhcpcd.conf"
result "dhcpcd's own settings before it are kept" "$?"
show 'cat /usr/share/monokulo/dhcpcd-nodo-local.conf' > "$tmp/block"
[ -s "$tmp/block" ] && [ "$(tail -n 1 "$tmp/dhcpcd.conf")" = '# END nodo-local' ] &&
	sed -n '/^# BEGIN nodo-local/,/^# END nodo-local/p' "$tmp/dhcpcd.conf" | sed '1d;$d' | cmp -s - "$tmp/block"
result "the block is monokulo-base's file, between its markers at the end" "$?"
if [ "$(grep -c '^# BEGIN nodo-local' "$tmp/dhcpcd.conf")" = 1 ]; then r=0; else r=1; fi
result "the marked block appears once" "$r"
show 'stat /usr/libexec/monokulo/dhcpcd-nodo-local' | grep -q 'Type: regular'
result "the script that keeps it there on updates is installed" "$?"
if [ "$(grep -cx 'interface nodo-local' "$tmp/dhcpcd.conf")" = 1 ]; then r=0; else r=1; fi
result "the block appears once" "$r"

echo "== cuprated"
show 'cat /etc/cuprated/Cuprated.toml' > "$tmp/Cuprated.toml"
python3 -I - "$tmp/Cuprated.toml" <<'PY'
import sys, tomllib
rpc = tomllib.load(open(sys.argv[1], "rb"))["rpc"]
r, u = rpc["restricted"], rpc["unrestricted"]
print(f"  restricted: enable={r.get('enable')} address={r.get('address')} port={r.get('port')} advertise={r.get('advertise')}")
print(f"  unrestricted: enable={u.get('enable')}")
assert r["enable"] is True and r["address"] == "127.0.0.1" and r["port"] == 18081 and r["advertise"] is False
assert u["enable"] is False
PY
result "RPC is restricted and on loopback only, and unrestricted RPC is off" "$?"
show 'cat /etc/nftables.d/cuprated.nft' > "$tmp/cuprated.nft"
sed -n '/chain input/,/}/p' "$tmp/cuprated.nft" | grep accept | tr -d '\t' > "$tmp/input"
[ "$(cat "$tmp/input")" = "tcp dport 18080 accept" ] && ! grep -q '1808[19]' "$tmp/cuprated.nft"
result "the firewall opens only P2P for cuprated, no RPC port" "$?"
show 'cat /etc/sv/cuprated/run' > "$tmp/run"
grep -qx '	--unshare-user --disable-userns \\' "$tmp/run" &&
	grep -qx '	/usr/bin/nodo-confine -- /usr/bin/cuprated --config-file /etc/cuprated/Cuprated.toml --skip-config-warning' "$tmp/run"
result "cuprated runs under nodo-confine in a user namespace with no room for another" "$?"
show 'stat /usr/bin/nodo-confine' | grep -q 'Type: regular'
result "nodo-confine is installed" "$?"
show 'cat /etc/sv/cuprated/conf' | grep -qx 'DATA_MOUNT=/media/monero'
result "cuprated still waits for the data drive" "$?"

echo "== data drive"
# No udev on the card, so no /dev/disk/by-partlabel link: the helper takes the drive from fstab
# and finds the partition itself (tests/data-drive.sh runs that in the VM).
show 'cat /etc/fstab' | grep -qx 'PARTLABEL=NODO_BLOCKCHAIN /media/monero xfs defaults,noatime,nofail 0 0'
result "fstab mounts the NODO_BLOCKCHAIN partition at /media/monero" "$?"
show 'cat /etc/sv/nodo-helper/run' > "$tmp/helper-run"
grep -q 'LABEL=NODO_BLOCKCHAIN' "$tmp/helper-run" && grep -q 'blkid -p -o value -s "$field"' "$tmp/helper-run" &&
	! show 'cat /etc/sv/nodo-helper/conf' | grep -q '^DRIVE'
result "the helper finds the data drive from fstab, with no conf overriding it" "$?"
show 'stat /usr/bin/udevd' | grep -q 'Type: regular' && echo "  note: the image has udevd, so /dev/disk links may exist too"

echo "== nodo-report"
show 'cat /usr/bin/nodo-report' > "$tmp/nodo-report"
grep -q 'section "abstract Unix sockets"' "$tmp/nodo-report" && grep -q 'nodo-healthd --abstract-sockets' "$tmp/nodo-report" &&
	grep -q 'nodo-healthd is not installed' "$tmp/nodo-report"
result "nodo-report lists abstract sockets, and copes without nodo-healthd" "$?"
show 'stat /usr/bin/nodo-healthd' | grep -q 'Type: regular'
result "nodo-healthd is installed for it" "$?"

echo "== the board's hardware: devices, console, clock, sound, GPU, radios"
r=0
for p in eudev alsa-utils wpa_supplicant bluez dbus v4l-utils; do
	grep -q "^$p " "$tmp/pkgs" || { echo "  not installed: $p"; r=1; }
done
result "eudev, alsa-utils, wpa_supplicant, bluez, dbus and v4l-utils are installed" "$r"
if grep -q '^mesa-dri ' "$tmp/pkgs"; then echo "  Mesa: $(grep -E '^(mesa|mesa-dri|libllvm[0-9]*) ' "$tmp/pkgs" | cut -d' ' -f2 | tr '\n' ' ')"; else echo "  Mesa: not installed"; fi
r=0
for s in udevd nodo-sound agetty-tty1; do
	case " $links " in *" $s "*) ;; *) echo "  not started: $s"; r=1 ;; esac
	show "stat /etc/sv/$s/run" | grep -qE 'Type: (regular|symlink)' || { echo "  no run script: $s"; r=1; }
done
result "udevd (module and firmware loading, hotplug), the mixer service and the tty1 login start at boot" "$r"
r=0
for s in wpa_supplicant bluetoothd dbus alsa; do
	case " $links " in *" $s "*) echo "  started at boot: $s"; r=1 ;; esac
done
show 'stat /etc/sv/wpa_supplicant/run' | grep -q 'Type: regular' && show 'stat /etc/sv/bluetoothd/run' | grep -q 'Type: regular' || r=1
result "Wi-Fi and Bluetooth services are installed but not started" "$r"
show 'cat /etc/udev/rules.d/70-nodo-radios-off.rules' | grep -q 'SUBSYSTEM=="rfkill", ATTR{type}=="wlan|bluetooth", ATTR{soft}="1"'
result "every Wi-Fi and Bluetooth radio is soft-blocked when it appears" "$?"
show 'stat /etc/udev/rules.d/80-net-name-slot.rules' | grep -q 'Fast link dest: "/dev/null"'
result "udev keeps the kernel's interface names (eth0, wlan0), which nothing in the firewall depends on" "$?"
show 'stat /etc/udev/hwdb.bin' | grep -q 'Type: regular'
result "the udev hardware database is compiled" "$?"
show 'cat /etc/udev/rules.d/60-nodo-gpu.rules' | grep -q 'SUBSYSTEM=="drm", KERNEL=="renderD\*", GROUP="video", MODE="0660"'
result "GPU nodes: card and render nodes only for the video group" "$?"
show 'cat /etc/rc.conf' | grep -qx 'HARDWARECLOCK="UTC"' && show 'cat /etc/runit/shutdown.d/40-hwclock.sh' | grep -q 'hwclock --systohc' &&
	show 'cat /etc/runit/core-services/05-nodo-rtc.sh' | grep -q 'hwclock --hctosys --utc'
result "the RTC sets the clock at boot and is written back at shutdown" "$?"
show 'cat /etc/sv/nodo-sound/run' | grep -q 'alsactl -f "$state" restore' && show 'cat /etc/sv/nodo-sound/finish' | grep -q 'alsactl -f /var/lib/alsa/asound.state store'
result "the mixer state is restored at boot and saved at shutdown" "$?"
show 'stat /usr/bin/ir-keytable' | grep -q 'Type: regular' && show 'stat /usr/bin/wpa_supplicant' | grep -q 'Type: regular' && show 'stat /usr/bin/bluetoothctl' | grep -q 'Type: regular'
result "ir-keytable, wpa_supplicant and bluetoothctl are there" "$?"
# Firmware: what the hardware asks for, uncompressed for the vendor kernel, and nothing else.
fw() { show "stat /usr/lib/firmware/$1" | grep -qE 'Type: (regular|symlink)'; }
r=0
for f in rtl_nic/rtl8125b-2.fw arm/mali/arch10.8/mali_csffw.bin arm/mali/arch10.10/mali_csffw.bin \
	rtw88/rtw8822c_fw.bin rtw89/rtw8852c_fw.bin rtl_bt/rtl8822cu_fw.bin rtl_bt/rtl8852cu_config.bin \
	mediatek/WIFI_RAM_CODE_MT7961_1.bin mediatek/BT_RAM_CODE_MT7922_1_1_hdr.bin mediatek/mt7925/WIFI_RAM_CODE_MT7925_1_1.bin \
	iwlwifi-cc-a0-77.ucode intel/iwlwifi/iwlwifi-ty-a0-gf-a0-89.ucode intel/ibt-0041-0041.sfi \
	ath11k/WCN6855/hw2.0/amss.bin ath12k/WCN7850/hw2.0/amss.bin qca/hpbtfw21.tlv; do
	fw "$f" || { echo "  missing firmware: $f"; r=1; }
done
result "firmware for the NIC, the Mali GPU and the Wi-Fi/Bluetooth chip families" "$r"
show 'stat /usr/lib/firmware/arm/mali/arch10.8/mali_csffw.bin' | grep -qE 'Fast link dest: "(/usr/lib/firmware/arm/mali|\.\.)/arch10\.10/mali_csffw\.bin"'
result "Mali's arch10.8 name links to the uncompressed arch10.10 file" "$?"
r=0
for f in iwlwifi-cc-a0-50.ucode brcm amdgpu nvidia ath10k mediatek/mt7996 intel/ibt-0180-0041.sfi \
	ath12k/WCN7850/hw2.0/ncm865 rtw89/rtw8922a_fw.bin rtw88/rtw8822c_fw.bin.zst arm/mali/arch10.8/mali_csffw.bin.zst \
	arm/mali/arch10.10/mali_csffw.bin.zst; do
	fw "$f" && { echo "  unexpected firmware: $f"; r=1; }
	show "stat /usr/lib/firmware/$f" | grep -q 'Type: directory' && { echo "  unexpected firmware: $f"; r=1; }
done
result "no firmware beyond the allow-list, and each file once, uncompressed" "$r"
echo "  firmware files: $(show 'ls -p /usr/lib/firmware' | wc -l) entries at the top level"
kver=$(show 'ls -p /usr/lib/modules' | tr '/' '\n' | grep -E '^[0-9]+\.[0-9]+\.' | sort -V | tail -1)
show "cat /usr/lib/modules/$kver/modules.dep" | cut -d: -f1 > "$tmp/modules"
if grep -q '^kernel/sound/' "$tmp/modules" && grep -q '^kernel/drivers/media/' "$tmp/modules"; then r=0; else r=1; fi
result "sound and media modules are on the card ($kver)" "$r"
grep -q '/btusb\.ko' "$tmp/modules"
result "btusb is there" "$?"
exit "$fail"
