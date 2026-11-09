#!/bin/sh
# Wi-Fi end to end inside the dev VM, as root, over the helper's real socket, with virtual radios:
# mac80211_hwsim makes two, and the second goes into its own network namespace with hostapd (WPA2)
# and dnsmasq as the access point and DHCP server. The helper and nodo-wifi drive the first one,
# which is the only radio the VM itself then sees. No real radio or network is involved.
# It changes the VM (radio state, the helper's Wi-Fi files), so run it on a disposable VM:
#   scripts/dev-vm.sh push nodo-tools
#   tests/wifi-prepare.sh                      (the hwsim module and the radio tools)
#   scripts/dev-vm.sh ssh 'cat > /tmp/wifi-hwsim.sh' < tests/wifi-hwsim.sh
#   scripts/dev-vm.sh ssh 'sh /tmp/wifi-hwsim.sh </dev/null'
set -u
fail=0
out=/tmp/wifi.out
pass() { echo "PASS $1"; }
bad() { echo "FAIL $1"; fail=1; sed 's/^/    /' "$out" | head -n 25; }
check() { name=$1; shift; if ( "$@" ) >"$out" 2>&1; then pass "$name"; else bad "$name"; fi; }
mkdir -p /tmp/wifi-bin /tmp/wifi-ap
# Commands rather than functions, so the sh -c bodies below can use them too.
# The answer on one line, as `Some( "x", )` for a value, so a pattern can span fields.
printf '%s\n' '#!/bin/sh' 'out=$(nodo-tui --ctl "$@" 2>&1); rc=$?' \
	'printf "%s\\n" "$out" | tr "\\n" " " | sed "s/  */ /g"; echo; exit $rc' > /tmp/wifi-bin/ctl
# Waits until the Wi-Fi status contains the text.
printf '%s\n' '#!/bin/sh' 'for _ in $(seq 1 40); do' \
	'	ctl wifi > /tmp/wifi.s 2>&1' \
	'	grep -q "$1" /tmp/wifi.s && { cat /tmp/wifi.s; exit 0; }' \
	'	sleep 1' 'done' 'cat /tmp/wifi.s' 'exit 1' > /tmp/wifi-bin/status_has
# The rfkill soft-block state of a radio, by its phy name.
printf '%s\n' '#!/bin/sh' \
	'for r in /sys/class/rfkill/rfkill*; do [ "$(cat "$r/name")" = "$1" ] && cat "$r/soft"; done' > /tmp/wifi-bin/rfkill_soft
chmod 755 /tmp/wifi-bin/ctl /tmp/wifi-bin/status_has /tmp/wifi-bin/rfkill_soft
PATH=/tmp/wifi-bin:$PATH
export PATH
wait_socket() { for _ in $(seq 1 30); do [ -S /run/nodo/helper.sock ] && ctl services >/dev/null 2>&1 && return 0; sleep 1; done; return 1; }
SSID=NodoTest
GOOD='correct horse battery'
WRONG='wrong horse battery'
AP=wifi-ap
in_ap() { ip netns exec "$AP" "$@"; }
phy_of() { basename "$(readlink -f "/sys/class/net/$1/phy80211")"; }
stop_ap() {
	[ -f /tmp/wifi-ap/hostapd.pid ] && kill "$(cat /tmp/wifi-ap/hostapd.pid)" 2>/dev/null
	[ -f /tmp/wifi-ap/dnsmasq.pid ] && kill "$(cat /tmp/wifi-ap/dnsmasq.pid)" 2>/dev/null
	rm -f /tmp/wifi-ap/*.pid
	ip netns del "$AP" 2>/dev/null
	sleep 1
	rmmod mac80211_hwsim 2>/dev/null
}

# A clean start, so the test can run again on the same VM.
[ -L /var/service/nodo-helper ] || ln -s /etc/sv/nodo-helper /var/service/
wait_socket && ctl wifi-on 0 >/dev/null 2>&1
stop_ap
rm -f /var/lib/nodo/wifi/networks.toml /var/lib/nodo/wifi/wpa_supplicant.conf
modprobe mac80211_hwsim radios=2 || { echo "FAIL mac80211_hwsim does not load: run tests/wifi-prepare.sh"; exit 1; }
CLIENT_PHY=$(phy_of wlan0)
AP_PHY=$(phy_of wlan1)

# The helper blocks a radio the owner left off when it starts, since rfkill state does not
# survive a reboot.
sv -w 30 restart nodo-helper >/dev/null
check "helper answers" wait_socket
check "Wi-Fi is off by default" sh -c 'ctl wifi | tee /dev/stderr | grep -q "enabled: false, link: Off"'
check "the helper blocked the radio at start" sh -c "[ \"\$(rfkill_soft $CLIENT_PHY)\" = 1 ]"
check "nodo-wifi is not running" sh -c '! pgrep -x wpa_supplicant'

# The access point, in its own namespace with its own radio, unblocked.
for r in /sys/class/rfkill/rfkill*; do [ "$(cat "$r/name")" = "$AP_PHY" ] && echo 0 > "$r/soft"; done
ip netns add "$AP"
iw phy "$AP_PHY" set netns name "$AP"
in_ap ip link set lo up
cat > /tmp/wifi-ap/hostapd.conf <<EOF
interface=wlan1
driver=nl80211
ssid=$SSID
hw_mode=g
channel=6
wpa=2
wpa_key_mgmt=WPA-PSK
rsn_pairwise=CCMP
wpa_passphrase=$GOOD
EOF
in_ap hostapd -B -P /tmp/wifi-ap/hostapd.pid /tmp/wifi-ap/hostapd.conf >/tmp/wifi-ap/hostapd.log 2>&1
in_ap ip addr add 192.168.77.1/24 dev wlan1
in_ap dnsmasq --conf-file=/dev/null --interface=wlan1 --bind-interfaces --port=0 \
	--dhcp-range=192.168.77.50,192.168.77.60,12h --dhcp-option=3,192.168.77.1 \
	--pid-file=/tmp/wifi-ap/dnsmasq.pid --dhcp-leasefile=/tmp/wifi-ap/leases
check "the access point is up in its namespace" sh -c "cat /tmp/wifi-ap/hostapd.log; ip netns exec $AP iw dev wlan1 info | grep -q 'ssid $SSID'"
check "the VM itself sees only the client radio" sh -c '[ -e /sys/class/net/wlan0 ] && [ ! -e /sys/class/net/wlan1 ]'

# On, scan, refuse, join.
check "turning Wi-Fi on starts wpa_supplicant and unblocks the radio" sh -c "
	ctl wifi-on 1 | tee /dev/stderr | grep -q 'enabled: true, link: Disconnected' &&
	pgrep -x wpa_supplicant && [ \"\$(rfkill_soft $CLIENT_PHY)\" = 0 ]"
check "the configuration and its folder are root-only" sh -c '
	[ "$(stat -c "%a %U" /var/lib/nodo/wifi)" = "700 root" ] &&
	[ "$(stat -c "%a %U" /var/lib/nodo/wifi/wpa_supplicant.conf)" = "600 root" ]'
check "the scan finds the access point with its security and signal" sh -c "
	for _ in 1 2 3; do ctl wifi-scan > /tmp/wifi.scan 2>&1; grep -q 'ssid: \"$SSID\"' /tmp/wifi.scan && break; done
	cat /tmp/wifi.scan; grep -q 'ssid: \"$SSID\", signal_dbm: -[0-9]*, frequency_mhz: 2437, security: Wpa2Personal' /tmp/wifi.scan"
check "a wrong passphrase is refused and nothing is saved" sh -c "
	echo '$WRONG' | ctl wifi-join $SSID wpa2 > /tmp/wifi.j 2>&1; cat /tmp/wifi.j
	grep -q '$SSID refused the passphrase. Nothing was saved.' /tmp/wifi.j &&
	! grep -q 'horse' /tmp/wifi.j && [ ! -e /var/lib/nodo/wifi/networks.toml ] &&
	ctl wifi | grep -q 'link: Disconnected'"
check "the right passphrase joins and DHCP gives an address" sh -c "
	echo '$GOOD' | ctl wifi-join $SSID wpa2 > /tmp/wifi.j 2>&1; cat /tmp/wifi.j
	grep -q 'link: Connected, ssid: Some( \"$SSID\"' /tmp/wifi.j &&
	grep -q 'address: Some( \"192.168.77.5[0-9]\"' /tmp/wifi.j &&
	ip -4 addr show wlan0 | grep -q 'inet 192.168.77.5'"
check "the status reports the signal" sh -c 'ctl wifi | tee /dev/stderr | grep -q "signal_dbm: Some( -[0-9]*,"'
check "the device reaches the access point over Wi-Fi" ping -c 2 -W 3 -I wlan0 192.168.77.1
check "only the derived key is stored, never the passphrase" sh -c "
	psk=\$(wpa_passphrase $SSID '$GOOD' | sed -n 's/^[[:space:]]*psk=\([0-9a-f]\{64\}\)\$/\1/p')
	[ -n \"\$psk\" ] && grep -q \"\$psk\" /var/lib/nodo/wifi/networks.toml &&
	grep -q \"psk=\$psk\" /var/lib/nodo/wifi/wpa_supplicant.conf &&
	! grep -rq 'horse' /var/lib/nodo/wifi"
check "no log holds a passphrase" sh -c "! grep -rq 'horse' /var/log/nodo-helper /var/log/nodo-wifi"
check "wired keeps the lower route metric" sh -c '
	ip route; wired=$(ip route show dev eth0 | sed -n "s/.* metric \([0-9]*\).*/\1/p" | sort -n | tail -1)
	wifi=$(ip route show dev wlan0 | sed -n "s/.* metric \([0-9]*\).*/\1/p" | sort -n | head -1)
	[ -n "$wired" ] && [ -n "$wifi" ] && [ "$wifi" -gt "$wired" ] && ip route show default dev wlan0 | grep -q "via 192.168.77.1"'
check "a saved network joins again after wpa_supplicant restarts" sh -c "
	sv -w 30 restart nodo-wifi >/dev/null && sleep 2 && status_has 'address: Some( \"192.168.77.5'"

# Forget, then off.
check "forgetting removes the network and leaves it" sh -c "
	ctl wifi-forget $SSID | tee /dev/stderr | grep -q 'saved: \[\]' &&
	! grep -q '$SSID' /var/lib/nodo/wifi/networks.toml &&
	! grep -q 'psk=' /var/lib/nodo/wifi/wpa_supplicant.conf &&
	ctl wifi | grep -q 'link: Disconnected'"
check "the address goes with the network" sh -c 'for _ in $(seq 1 20); do ip -4 addr show wlan0 | grep -q "inet 192.168.77" || exit 0; sleep 1; done; exit 1'
check "turning Wi-Fi off stops wpa_supplicant and blocks the radio" sh -c "
	ctl wifi-on 0 | tee /dev/stderr | grep -q 'enabled: false, link: Off' &&
	! pgrep -x wpa_supplicant && [ \"\$(rfkill_soft $CLIENT_PHY)\" = 1 ]"
check "Wi-Fi stays off across a helper restart" sh -c '
	[ -e /etc/sv/nodo-wifi/down ] && sv -w 30 restart nodo-helper >/dev/null && sleep 2 &&
	ctl wifi | grep -q "enabled: false, link: Off" && ! pgrep -x wpa_supplicant'

# Leaves the VM as it was: no access point, no virtual radios.
stop_ap
if [ "$fail" = 0 ]; then echo "wifi-hwsim: all passed"; else echo "wifi-hwsim: FAILED"; fi
exit "$fail"
