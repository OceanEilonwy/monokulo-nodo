#!/bin/sh
# Reads a finished T6N card image (either kernel) as plain files, with no loop devices and no
# root, and checks what the image adds on top of the boot path: the platform packages and the
# services started at boot, nodo-localnet's dhcpcd block, restricted RPC on loopback, cuprated
# under nodo-confine, nodo-report's abstract socket list, and the GPU's group, udev rule and
# (on the mainline card only) Vulkan, and the default app feed with its pinned key. The image is
# only read.
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
grep -E '^(nodo-base|cuprated|cuprated-service|nodo-tools|nodo-caddy|nodo-ddns|nodo-localnet|linux-nanopc-t6n|linux-mainline-rk3588) ' "$tmp/pkgs" | sed 's/^/  /'
for p in nodo-tools nodo-caddy nodo-ddns nodo-localnet cuprated-service nodo-base; do
	grep -q "^$p " "$tmp/pkgs"
	result "$p is installed" "$?"
done
! grep -q '^mk-pay ' "$tmp/pkgs"
result "Monokulo (mk-pay) is not preinstalled; it comes through the helper from a feed" "$?"
# The app feed is a default source with its key pinned from the start; the repository's file
# is tied to feed/signing-key.pub.pem by nodo-helper's tests.
repo=$(cd "$(dirname "$0")/.." && pwd)
want=$(sed -n 's/^fingerprint *= *"\(.*\)"/\1/p' "$repo/packages/nodo-tools/files/default-feeds.toml")
show 'cat /usr/share/nodo/default-feeds.toml' > "$tmp/default-feeds.toml"
got=$(sed -n 's/^fingerprint *= *"\(.*\)"/\1/p' "$tmp/default-feeds.toml")
echo "== default feed: $(sed -n 's/^address *= *"\(.*\)"/\1/p' "$tmp/default-feeds.toml") key $got"
grep -qx 'address = "https://nodo.monokulo.com/feed"' "$tmp/default-feeds.toml" && [ -n "$got" ] && [ "$got" = "$want" ]
result "the app feed nodo.monokulo.com/feed ships as a default, pinned to $want" "$?"
# System snapshots (docs/UPDATES.md, section 1): the update server and the keys a snapshot must be
# signed with, exactly as the repository has them.
show 'cat /etc/nodo/system-update.toml' > "$tmp/system-update.toml"
show 'cat /usr/share/nodo/snapshot-pins.toml' > "$tmp/snapshot-pins.toml"
r=0
grep -qx 'server = "https://nodo.monokulo.com/snapshots"' "$tmp/system-update.toml" || { echo "  no update server"; r=1; }
cmp -s "$tmp/snapshot-pins.toml" "$repo/packages/nodo-tools/files/snapshot-pins.toml" || { echo "  the snapshot keys differ from the repository's"; r=1; }
result "system snapshots come from nodo.monokulo.com/snapshots, signed with the pinned keys" "$r"

# sshd refuses root when root's shell is missing; the first T6N cards had /bin/bash without bash.
root_shell=$(show 'cat /etc/passwd' | awk -F: '$1 == "root" { print $7 }')
echo "== root's shell: $root_shell"
if [ "$root_shell" = /bin/sh ]; then r=0; else r=1; fi
result "root's login shell is /bin/sh" "$r"
# /bin and /usr/bin/sh are symlinks (to usr/bin and dash); stat on the target the link names.
show 'stat /usr/bin/dash' | grep -q 'Type: regular'
result "the shell it points to, dash, is installed" "$?"

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
result "the block is nodo-base's file, between its markers at the end" "$?"
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
# An ordinary shutdown (poweroff over SSH, a critical temperature) waits for cuprated to save before
# runit's general stop, which kills a service after 7 seconds.
r=0
show 'stat /etc/runit/shutdown.d/06-nodo-long-stop.sh' | grep -q 'Type: regular' || { echo "  missing: /etc/runit/shutdown.d/06-nodo-long-stop.sh"; r=1; }
show 'cat /etc/runit/shutdown.d/06-nodo-long-stop.sh' | grep -q 'nodo-power --before-shutdown' || { echo "  the hook does not run nodo-power --before-shutdown"; r=1; }
show 'stat /etc/runit/shutdown.d/10-sv-stop.sh' | grep -q 'Type: regular' || { echo "  Void's 10-sv-stop.sh is not where the hook expects it, after 06"; r=1; }
result "shutdown waits for cuprated to save before runit's 7-second force-stop" "$r"

echo "== data drive"
# No udev on the card, so no /dev/disk/by-partlabel link: the helper takes the drive from fstab
# and finds the partition itself (tests/data-drive.sh runs that in the VM).
show 'cat /etc/fstab' | grep -qx 'PARTLABEL=NODO_BLOCKCHAIN /media/monero xfs defaults,noatime,nofail 0 0'
result "fstab mounts the NODO_BLOCKCHAIN partition at /media/monero" "$?"
show 'cat /etc/sv/nodo-helper/run' > "$tmp/helper-run"
grep -q 'LABEL=NODO_BLOCKCHAIN' "$tmp/helper-run" && grep -q 'blkid -p -o value -s "$field"' "$tmp/helper-run" &&
	! show 'cat /etc/sv/nodo-helper/conf' | grep -q '^DRIVE'
result "the helper finds the data drive from fstab, with no conf overriding it" "$?"
grep -q -- "--screen-uid" "$tmp/helper-run" && grep -q "id -u _nodo_screen" "$tmp/helper-run"
result "the helper gives the touchscreen its own socket, limited by the screen PIN" "$?"
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
# A card with the screen's panels gives them the screen (nodo-gui, or nodo-screen in text on tty1);
# any other keeps tty1's login prompt.
case " $links " in *" nodo-gui "*) tty1=nodo-gui ;; *" nodo-screen "*) tty1=nodo-screen ;; *) tty1=agetty-tty1 ;; esac
r=0
for s in udevd nodo-sound $tty1; do
	case " $links " in *" $s "*) ;; *) echo "  not started: $s"; r=1 ;; esac
	show "stat /etc/sv/$s/run" | grep -qE 'Type: (regular|symlink)' || { echo "  no run script: $s"; r=1; }
done
result "udevd (module and firmware loading, hotplug), the mixer service and tty1 ($tty1) start at boot" "$r"
if [ "$tty1" != agetty-tty1 ]; then
	r=0
	case " $links " in *" agetty-tty1 "*) echo "  tty1's login prompt also starts"; r=1 ;; esac
	case " $links " in *" nodo-gui "*" nodo-screen "*|*" nodo-screen "*" nodo-gui "*) echo "  both nodo-gui and nodo-screen start"; r=1 ;; esac
	if [ "$tty1" = nodo-gui ]; then
		screen_files="/usr/bin/nodo-gui /usr/share/fonts/nodo/JetBrainsMono-Regular.ttf /usr/share/fonts/nodo/JetBrainsMono-Bold.ttf /usr/share/licenses/nodo-tools/OFL-JetBrainsMono.txt"
	else
		screen_files="/usr/bin/setfont /usr/share/nodo/nodo-screen-16x32.psf.gz /usr/share/licenses/nodo-tools/OFL-nodo-screen-16x32.txt"
	fi
	# nodo-screen stays installed either way, for terminals over SSH.
	for f in /usr/bin/nodo-screen $screen_files /usr/share/nodo/packages/cuprated/panels/sync.lua; do
		show "stat $f" | grep -q 'Type: regular' || { echo "  missing: $f"; r=1; }
	done
	show 'cat /etc/passwd' | grep -q '^_nodo_screen:' || { echo "  no _nodo_screen account"; r=1; }
	show 'cat /etc/group' | grep -qE '^input:[^:]*:[0-9]+:(.*,)?_nodo_screen(,|$)' || { echo "  _nodo_screen is not in input"; r=1; }
	if [ "$tty1" = nodo-gui ]; then
		# The display: chpst gives nodo-gui the video group whatever /etc/group says.
		show 'cat /etc/sv/nodo-gui/run' | grep -q 'chpst -u _nodo_screen:_nodo_screen:input:video /usr/bin/nodo-gui' ||
			{ echo "  nodo-gui does not run as _nodo_screen with input and video only"; r=1; }
		show 'cat /etc/sv/nodo-gui/run' | grep -q 'SLINT_KMS_ROTATION=270' || { echo "  nodo-gui is not turned to the case"; r=1; }
	fi
	show 'cat /usr/share/nodo/manifests/cuprated.toml' | grep -q 'method = "get_info"' || { echo "  cuprated offers no panel"; r=1; }
	result "the screen's panels replace tty1's login, with their font, account and cuprated's panel" "$r"
	# The screen's Shut down button asks nodo-power, the one root service that may turn the device off.
	r=0
	case " $links " in *" nodo-power "*) ;; *) echo "  not started: nodo-power"; r=1 ;; esac
	show 'stat /usr/bin/nodo-power' | grep -q 'Type: regular' || { echo "  missing: /usr/bin/nodo-power"; r=1; }
	show 'cat /etc/sv/nodo-power/run' | grep -q 'install -d -m 0750 -o root -g _nodo_screen /run/nodo-power' ||
		{ echo "  nodo-power's socket directory is not root:_nodo_screen 0750"; r=1; }
	show "cat /etc/sv/$tty1/finish" | grep -q '/run/nodo-power/off' || { echo "  the screen's finish clears Safe to unplug"; r=1; }
	show "cat /etc/sv/$tty1/run" | grep -q 'chgrp _nodo_screen "$b" && chmod g+w "$b"' || { echo "  the screen cannot dim the backlight"; r=1; }
	result "the screen's Shut down button has nodo-power to ask (only root and _nodo_screen may), and the screen may dim its backlight" "$r"
	r=0
	for f in /usr/bin/nodo-splash /etc/runit/core-services/01-nodo-splash.sh /etc/runit/shutdown.d/05-nodo-splash.sh; do
		show "stat $f" | grep -q 'Type: regular' || { echo "  missing: $f"; r=1; }
	done
	show "cat /etc/sv/$tty1/run" | grep -q /run/nodo-splash.pid || { echo "  $tty1 does not stop the splash"; r=1; }
	result "the boot splash shows the logo from early boot until the panels, and again at shutdown" "$r"
	r=0
	show "cat /etc/profile.d/nodo-tui.sh" | grep -q SSH_TTY || { echo "  missing: /etc/profile.d/nodo-tui.sh"; r=1; }
	show "cat /etc/profile" | grep -q "/etc/profile.d/" || { echo "  /etc/profile does not read /etc/profile.d"; r=1; }
	result "an interactive SSH login opens nodo-tui" "$r"
	r=0
	for f in /usr/libexec/nodo/nodo-fixed-mac /etc/runit/core-services/03-nodo-fixed-mac.sh; do
		show "stat $f" | grep -q 'Type: regular' || { echo "  missing: $f"; r=1; }
	done
	result "Ethernet gets a fixed address from the board serial before dhcpcd starts" "$r"
	# This card takes base updates as system snapshots; the timer only checks unless the owner
	# turns automatic system updates on.
	case " $links " in *" nodo-systemupdate "*) r=0 ;; *) r=1 ;; esac
	result "the system snapshot timer (nodo-systemupdate) starts at boot" "$r"
fi
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
show 'cat /etc/udev/rules.d/60-nodo-gpu.rules' | grep -v '^#' > "$tmp/gpu.rules"
if [ "$(cat "$tmp/gpu.rules")" = 'SUBSYSTEM=="drm", KERNEL=="renderD*", GROUP="gpu", MODE="0660"' ]; then r=0; else r=1; fi
result "GPU render nodes belong to the gpu group (0660); card nodes keep eudev's video group" "$r"
show 'cat /etc/group' > "$tmp/group"
gpu_line=$(grep '^gpu:' "$tmp/group")
echo "  $gpu_line"
if [ -n "$gpu_line" ] && [ -z "$(echo "$gpu_line" | cut -d: -f4)" ]; then r=0; else r=1; fi
result "the gpu group exists and has no members; a service gets it only with the owner's approval" "$r"
show 'stat /usr/bin/nodo-gpu-check' | grep -q 'Type: regular' && grep -q 'nodo-gpu-check' "$tmp/nodo-report"
result "nodo-gpu-check is installed and nodo-report runs it" "$?"
# Vulkan through panvk needs panthor, which only the mainline kernel has.
icd=/usr/share/vulkan/icd.d/panfrost_icd.aarch64.json
if grep -q '^linux-mainline-rk3588 ' "$tmp/pkgs"; then
	echo "  Vulkan: $(grep -E '^(vulkan-loader|mesa-panvk) ' "$tmp/pkgs" | cut -d' ' -f2 | tr '\n' ' ')"
	grep -q '^vulkan-loader ' "$tmp/pkgs" && grep -q '^mesa-panvk ' "$tmp/pkgs"
	result "the mainline card has the Vulkan loader and panvk" "$?"
	show "cat $icd" > "$tmp/icd.json"
	python3 -I - "$tmp/icd.json" <<'PY'
import json, sys
icd = json.load(open(sys.argv[1]))["ICD"]
print(f"  ICD: {icd['library_path']}, API {icd['api_version']}")
assert icd["library_path"] == "/usr/lib/libvulkan_panfrost.so" and icd["api_version"].startswith("1.4.")
PY
	result "panvk's ICD manifest points the loader at libvulkan_panfrost.so" "$?"
	show 'stat /usr/lib/libvulkan_panfrost.so' | grep -q 'Type: regular' && show 'stat /usr/lib/libvulkan.so.1' | grep -qE 'Type: (regular|symlink)'
	result "libvulkan_panfrost.so and libvulkan.so.1 are installed" "$?"
else
	r=0
	for p in vulkan-loader mesa-panvk; do grep -q "^$p " "$tmp/pkgs" && { echo "  installed: $p"; r=1; }; done
	show "stat $icd" | grep -q 'Type:' && { echo "  present: $icd"; r=1; }
	show 'stat /usr/lib/libvulkan_panfrost.so' | grep -q 'Type:' && { echo "  present: libvulkan_panfrost.so"; r=1; }
	result "the vendor card has no Vulkan: its kernel's Mali driver is kbase, which panvk cannot use" "$r"
fi
r=0
for p in mesa mesa-dri mesa-vulkan-panfrost libllvm22 libX11 wayland; do grep -q "^$p " "$tmp/pkgs" && { echo "  installed: $p"; r=1; }; done
result "no Void Mesa, LLVM, X11 or Wayland on the card" "$r"
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
