#!/bin/sh
# Puts what tests/wifi-hwsim.sh needs into the running dev VM: virtual Wi-Fi radios and the radio
# tools. Runs on the host, for the x86-kvm board.
#
# Void's kernel is built without mac80211_hwsim, and the KVM image leaves the wireless stack out
# (boards/x86-kvm/board.env). So this builds mac80211_hwsim as an out-of-tree module, from the
# stable kernel's source of the same version (pinned by SHA-256 below), against the
# linux-headers package of the VM's kernel, in a throwaway builder container. cfg80211 and
# mac80211 come from the VM kernel's own package on the pinned mirror. Everything is copied into
# the VM's /usr/lib/modules/<kernel>/extra, never onto the host. The module is unsigned, which the
# kernel accepts (CONFIG_MODULE_SIG_FORCE is off) and marks as a taint.
#
# hostapd, wpa_supplicant, dnsmasq, iw, bluez and iputils (for ping) come from the pinned mirror.
#   tests/wifi-prepare.sh, then tests/wifi-hwsim.sh as its header says
set -eu
root=$(cd "$(dirname "$0")/.." && pwd)
. "$root/scripts/lib.sh"
vm() { "$root/scripts/dev-vm.sh" "$@"; }

# The source of the module at the VM kernel's upstream version.
HWSIM_C_SHA256=ff2e7c132da7331e223ac276ce4cf6fa5699b5b47e56a9584a614dbac93c97cd
HWSIM_H_SHA256=8a048599258fd34474b21a37106bd043dc1e9bdfde5a711040fb5d1f6de4d75d

kver=$(vm ssh 'uname -r')
upstream=${kver%_*}
series=$(printf %s "$upstream" | cut -d. -f1-2)
[ "$upstream" = 6.18.62 ] || { echo "the pinned hwsim source is for 6.18.62, the VM runs $kver" >&2; exit 1; }
out=$root/build/hwsim/$kver
mkdir -p "$out"

if [ ! -s "$out/mac80211_hwsim.ko" ] || [ ! -s "$out/mac80211.ko.zst" ]; then
	ensure_builder
	src=$out/src
	mkdir -p "$src"
	for f in mac80211_hwsim.c mac80211_hwsim.h; do
		case $f in *.c) sum=$HWSIM_C_SHA256 ;; *) sum=$HWSIM_H_SHA256 ;; esac
		if ! printf '%s  %s\n' "$sum" "$src/$f" | sha256sum -c - >/dev/null 2>&1; then
			curl -fsS -o "$src/$f.part" "https://git.kernel.org/pub/scm/linux/kernel/git/stable/linux.git/plain/drivers/net/wireless/virtual/$f?h=v$upstream"
			printf '%s  %s\n' "$sum" "$src/$f.part" | sha256sum -c - >/dev/null || { echo "checksum mismatch: $f" >&2; rm -f "$src/$f.part"; exit 1; }
			mv "$src/$f.part" "$src/$f"
		fi
	done
	printf 'obj-m := mac80211_hwsim.o\n' > "$src/Kbuild"
	# Only this container is ever stopped, by its exact name.
	cname=monokulo-hwsim-build-$(printf %s "$root" | sha256sum | cut -c1-8)
	docker rm -f "$cname" >/dev/null 2>&1 || true
	docker run --rm --name "$cname" -v "$out:/out" "$BUILDER" sh -euc '
		xbps-install -Sy "linux$1-headers-$2" gcc make elfutils-devel >/dev/null
		xbps-install -Dy -c /tmp/pkg "linux$1-$2" >/dev/null
		cp -r /out/src /tmp/hwsim
		make -s -C "/usr/src/kernel-headers-$2" M=/tmp/hwsim modules
		cp /tmp/hwsim/mac80211_hwsim.ko /out/
		m=./usr/lib/modules/$2/kernel
		mkdir /tmp/k
		zstd -dc /tmp/pkg/linux$1-$2.*.xbps | tar -C /tmp/k -x "$m/net/wireless/cfg80211.ko.zst" "$m/net/mac80211/mac80211.ko.zst" "$m/lib/crypto/libarc4.ko.zst"
		find /tmp/k -name "*.ko.zst" -exec cp {} /out/ \;
		chown -R "$3" /out
	' sh "$series" "$kver" "$(id -u):$(id -g)"
fi

vm mirror hostapd wpa_supplicant dnsmasq iw bluez python3 iputils
tar -C "$out" -cf - mac80211_hwsim.ko cfg80211.ko.zst mac80211.ko.zst libarc4.ko.zst |
	vm ssh "mkdir -p /usr/lib/modules/$kver/extra && tar -C /usr/lib/modules/$kver/extra -xf - && depmod -a $kver"
vm ssh 'modinfo -F vermagic mac80211_hwsim'
