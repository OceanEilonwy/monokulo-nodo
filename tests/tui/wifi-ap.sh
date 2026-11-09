#!/bin/sh
# A virtual Wi-Fi access point for the Network journey, inside the VM: mac80211_hwsim makes two
# radios, and the second goes into its own network namespace with hostapd (WPA2, the passphrase
# on stdin) and dnsmasq. The VM itself then sees only the first, which the helper drives.
# tests/wifi-prepare.sh puts the module and the tools in the VM.
# The radios come first, so the helper can be restarted to block the one it drives before the
# access point unblocks its own.
#   wifi-ap.sh radios | wifi-ap.sh up SSID < passphrase | wifi-ap.sh down
set -eu
ns=wifi-ap
dir=/tmp/wifi-ap
down() {
	for f in "$dir/hostapd.pid" "$dir/dnsmasq.pid"; do
		if [ -f "$f" ]; then kill "$(cat "$f")" 2>/dev/null || true; fi
	done
	rm -f "$dir"/*.pid "$dir/hostapd.conf"
	ip netns del "$ns" 2>/dev/null || true
	sleep 1
	rmmod mac80211_hwsim 2>/dev/null || true
}
case ${1:-} in
radios)
	down
	modprobe mac80211_hwsim radios=2
	;;
up)
	ssid=$2
	IFS= read -r passphrase
	mkdir -p "$dir"
	phy=$(basename "$(readlink -f /sys/class/net/wlan1/phy80211)")
	for r in /sys/class/rfkill/rfkill*; do
		if [ "$(cat "$r/name")" = "$phy" ]; then echo 0 > "$r/soft"; fi
	done
	ip netns add "$ns"
	iw phy "$phy" set netns name "$ns"
	ip netns exec "$ns" ip link set lo up
	umask 077
	printf 'interface=wlan1\ndriver=nl80211\nssid=%s\nhw_mode=g\nchannel=6\nwpa=2\nwpa_key_mgmt=WPA-PSK\nrsn_pairwise=CCMP\nwpa_passphrase=%s\n' \
		"$ssid" "$passphrase" > "$dir/hostapd.conf"
	ip netns exec "$ns" hostapd -B -P "$dir/hostapd.pid" "$dir/hostapd.conf" >/dev/null
	ip netns exec "$ns" ip addr add 192.168.77.1/24 dev wlan1
	ip netns exec "$ns" dnsmasq --conf-file=/dev/null --interface=wlan1 --bind-interfaces --port=0 \
		--dhcp-range=192.168.77.50,192.168.77.60,12h --dhcp-option=3,192.168.77.1 \
		--pid-file="$dir/dnsmasq.pid" --dhcp-leasefile="$dir/leases"
	# hostapd's file holds the passphrase, so it goes once hostapd has read it.
	rm -f "$dir/hostapd.conf"
	echo ready
	;;
down) down ;;
*) echo "usage: wifi-ap.sh radios | up SSID < passphrase | down" >&2; exit 2 ;;
esac
