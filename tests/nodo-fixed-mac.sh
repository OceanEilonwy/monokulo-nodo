#!/bin/sh
# nodo-fixed-mac: same address for the same board every time, only for Ethernet cards without a
# burned-in address, unicast and locally administered. Runs on the host with a fake /sys and /proc.
set -eu
here=$(cd "$(dirname "$0")/.." && pwd)
script=$here/packages/nodo-tools/files/network/nodo-fixed-mac
t=$(mktemp -d); trap 'rm -rf "$t"' EXIT
card() { # name address type [wireless]
	mkdir -p "$t/sys/class/net/$1/device"
	echo "$2" >"$t/sys/class/net/$1/address"; echo "$3" >"$t/sys/class/net/$1/type"
	[ -z "${4:-}" ] || mkdir -p "$t/sys/class/net/$1/wireless"
}
mkdir -p "$t/proc/device-tree" "$t/sys/class/net/lo"
printf '90f7d6ac24e776a4\0' >"$t/proc/device-tree/serial-number"
echo 00:00:00:00:00:00 >"$t/sys/class/net/lo/address"; echo 772 >"$t/sys/class/net/lo/type"
card eth0 b6:ce:0d:a3:fd:25 1            # random (locally administered): replaced
card eth1 00:e0:4c:68:01:02 1            # burned-in (global): kept
card wlan0 3a:11:22:33:44:55 1 wireless  # Wi-Fi: left alone
out=$(NODO_ROOT=$t sh "$script" --print)
again=$(NODO_ROOT=$t sh "$script" --print)
[ "$out" = "$again" ] || { echo "FAIL: not stable: $out / $again"; exit 1; }
[ "$(printf '%s\n' "$out" | wc -l)" = 1 ] || { echo "FAIL: expected only eth0, got: $out"; exit 1; }
mac=${out#eth0 }
printf '%s\n' "$mac" | grep -Eq '^[0-9a-f]{2}(:[0-9a-f]{2}){5}$' || { echo "FAIL: bad address $mac"; exit 1; }
b0=$(printf '%d' "0x${mac%%:*}")
[ $((b0 & 1)) -eq 0 ] || { echo "FAIL: multicast bit set in $mac"; exit 1; }
[ $((b0 & 2)) -ne 0 ] || { echo "FAIL: not locally administered: $mac"; exit 1; }
printf 'ffffffffffffffff\0' >"$t/proc/device-tree/serial-number"
other=$(NODO_ROOT=$t sh "$script" --print)
[ "$other" != "$out" ] || { echo "FAIL: different boards got the same address"; exit 1; }
rm "$t/proc/device-tree/serial-number"
[ -z "$(NODO_ROOT=$t sh "$script" --print 2>/dev/null)" ] || { echo "FAIL: changed addresses without a serial"; exit 1; }
echo "PASS nodo-fixed-mac ($mac)"
