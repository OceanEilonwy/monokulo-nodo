#!/bin/sh
# Runs on the device under test with nodo-localnet installed. Checks that the local address comes from
# a second DHCP lease on a macvlan interface with its own MAC address, from QEMU's DHCP server on eth0,
# and that every way of not getting one is reported as unavailable with a reason.
set -u
fail=0
state=/run/nodo-localnet/state
result() { if [ "$2" = 0 ]; then echo "PASS $1"; else echo "FAIL $1"; fail=1; fi; }
# Services stay linked between runs and are only switched down, with a down file so a reboot keeps
# them down. Removing a link makes runsv linger while its log runs, and a lingering one ignores the
# next start.
service_up() {
	rm -f "/etc/sv/$1/down"
	ln -sfn "/etc/sv/$1" "/etc/runit/runsvdir/default/$1"
	i=0; until sv -w 5 up "$1" >/dev/null 2>&1 || [ "$i" -ge 10 ]; do i=$((i+1)); sleep 2; done
}
service_down() {
	[ -d "/etc/sv/$1" ] || return 0
	touch "/etc/sv/$1/down"
	sv -w 15 down "$1" >/dev/null 2>&1
}
cleanup() {
	[ -n "${dhcpd_pid:-}" ] && kill "$dhcpd_pid" 2>/dev/null
	service_down nodo-localnet
	rm -f /etc/sv/nodo-localnet/conf
	ip link del lnhost 2>/dev/null
	ip netns del lnpeer 2>/dev/null
}
trap cleanup EXIT
run_with() {
	printf '%s\n' "$@" > /etc/sv/nodo-localnet/conf
	sv -w 20 down nodo-localnet >/dev/null 2>&1
	rm -f "$state"
	service_up nodo-localnet
}
wait_status() { i=0; until grep -q "^status=$1" "$state" 2>/dev/null || [ "$i" -ge "$2" ]; do i=$((i+1)); sleep 1; done; grep -q "^status=$1" "$state"; }

# monokulo-base puts its block into dhcpcd's own configuration, between markers.
grep -qx 'interface nodo-local' /etc/dhcpcd.conf && grep -A12 -x 'interface nodo-local' /etc/dhcpcd.conf | grep -qx '	nogateway' &&
	grep -q '^# BEGIN nodo-local' /etc/dhcpcd.conf
result "dhcpcd.conf has the nodo-local block" "$?"

run_with MODE=dhcp PARENT=eth0 LEASE_WAIT=30
# The run script removes a default route every 10 seconds anyway. Watch closely from the start
# until well after the lease, so a route dhcpcd added itself would be seen before that.
: > /tmp/ln-routes
i=0; seen=0
while [ "$i" -lt 160 ]; do
	ip -4 route show default dev nodo-local 2>/dev/null >> /tmp/ln-routes
	if grep -q '^status=ready' "$state" 2>/dev/null; then seen=$((seen+1)); [ "$seen" -ge 30 ] && break; fi
	i=$((i+1)); sleep 0.5
done
grep -q '^status=ready' "$state"
result "macvlan gets its own lease" "$?"
if [ -s /tmp/ln-routes ]; then r=1; else r=0; fi
result "dhcpcd never adds a default route on the local interface" "$r"
ip -4 route show dev nodo-local | grep -q 'proto dhcp.* metric 4000'
result "its subnet route ranks below the main interface's" "$?"
! find /run/dhcpcd/hook-state/resolv.conf -name 'nodo-local*' 2>/dev/null | grep -q . && ! grep -q nodo-local /etc/resolv.conf
result "the local lease adds no DNS servers" "$?"
! ip -6 addr show dev nodo-local | grep inet6 | grep -qv 'scope link'
result "the local interface gets no IPv6 address beyond link-local" "$?"
address=$(sed -n 's/^address=//p' "$state")
main=$(ip -4 -o addr show dev eth0 | awk '{ sub("/.*", "", $4); print $4; exit }')
case $address in 10.0.2.*) [ "$address" != "$main" ] ;; *) false ;; esac
result "the local address ($address) differs from the main address ($main)" "$?"
mac=$(cat /sys/class/net/nodo-local/address)
[ "$mac" = "$(cat /var/lib/nodo-localnet/mac)" ] && [ "$mac" != "$(cat /sys/class/net/eth0/address)" ]
result "the local interface has its own stable MAC address" "$?"
! ip -4 route show default dev nodo-local | grep -q .
result "no default route leaves from the local address" "$?"
sv -w 20 stop nodo-localnet >/dev/null
! ip link show nodo-local >/dev/null 2>&1 && ! grep -q '^status=ready' "$state"
result "stopping removes the local interface" "$?"
run_with MODE=dhcp PARENT=eth0 LEASE_WAIT=30
wait_status ready 60 && [ "$(sed -n 's/^address=//p' "$state")" = "$address" ]
result "the same lease comes back after a restart" "$?"

# A LAN with no DHCP server: the lease is reported as refused, never as ready.
ip netns add lnpeer
ip link add lnhost type veth peer name eth0 netns lnpeer
ip addr add 192.168.78.2/24 dev lnhost
ip link set lnhost up
ip netns exec lnpeer ip link set eth0 up
run_with MODE=dhcp PARENT=lnhost LEASE_WAIT=10
wait_status no-lease 40
result "no second lease is reported as no-lease" "$?"
! ip -4 addr show dev nodo-local | grep -q 'inet 169\.254\.'
result "no 169.254 fallback address without a lease" "$?"

# QEMU's offline network hands out no router or DNS server, so a small DHCP server on the veth
# peer does, and also a host name and an MTU. It answers only the local interface's MAC address.
# Nothing from that lease but the address may reach the device.
if command -v python3 >/dev/null; then
	cat > /tmp/ln-dhcpd.py <<'EOF'
import socket, struct, sys
ifname, mac = sys.argv[1], bytes.fromhex(sys.argv[2].replace(":", ""))
server, offer = socket.inet_aton("192.168.78.1"), socket.inet_aton("192.168.78.50")
s = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
s.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
s.setsockopt(socket.SOL_SOCKET, socket.SO_BROADCAST, 1)
s.setsockopt(socket.SOL_SOCKET, socket.SO_BINDTODEVICE, ifname.encode())
s.bind(("", 67))
def opt(code, data):
    return bytes([code, len(data)]) + data
while True:
    req, _ = s.recvfrom(1500)
    if len(req) < 240 or req[28:34] != mac or req[236:240] != b"\x63\x82\x53\x63":
        continue
    kind, i = None, 240
    while i < len(req) and req[i] != 255:
        if req[i] == 0:
            i += 1
            continue
        if req[i] == 53:
            kind = req[i + 2]
        i += 2 + req[i + 1]
    reply = {1: 2, 3: 5}.get(kind)
    if reply is None:
        continue
    head = struct.pack("!BBBB4s2s2s4s4s4s4s16s", 2, 1, 6, 0, req[4:8], b"\0\0", req[10:12],
                       b"\0" * 4, offer, server, b"\0" * 4, req[28:44])
    opts = (opt(53, bytes([reply])) + opt(54, server) + opt(51, struct.pack("!I", 3600)) +
            opt(1, socket.inet_aton("255.255.255.0")) + opt(3, server) + opt(6, server) +
            opt(15, b"intruder.test") + opt(12, b"intruder") + opt(26, struct.pack("!H", 1280)) +
            opt(121, bytes([8, 10, 192, 168, 78, 1])) + b"\xff")
    s.sendto(head + b"\0" * 192 + b"\x63\x82\x53\x63" + opts, ("255.255.255.255", 68))
EOF
	ip netns exec lnpeer ip addr add 192.168.78.1/24 dev eth0
	ip netns exec lnpeer python3 /tmp/ln-dhcpd.py eth0 "$(cat /var/lib/nodo-localnet/mac)" </dev/null >/dev/null 2>&1 &
	dhcpd_pid=$!
	host_name=$(hostname)
	run_with MODE=dhcp PARENT=lnhost LEASE_WAIT=30
	: > /tmp/ln-leaked
	i=0; seen=0
	while [ "$i" -lt 160 ]; do
		ip -4 route show default dev nodo-local 2>/dev/null >> /tmp/ln-leaked
		ip -4 route show 10.0.0.0/8 dev nodo-local 2>/dev/null >> /tmp/ln-leaked
		grep -h '192\.168\.78\.1\|intruder' /etc/resolv.conf >> /tmp/ln-leaked
		if grep -q '^status=ready' "$state" 2>/dev/null; then seen=$((seen+1)); [ "$seen" -ge 30 ] && break; fi
		i=$((i+1)); sleep 0.5
	done
	grep -qx 'address=192.168.78.50' "$state"
	result "a lease from a server that also offers a router and DNS is ready" "$?"
	sort -u /tmp/ln-leaked | sed 's/^/    leaked: /'
	if [ -s /tmp/ln-leaked ]; then r=1; else r=0; fi
	result "its router, static routes, DNS server and domain never reach the device" "$r"
	if [ "$(hostname)" = "$host_name" ] && [ "$(cat /sys/class/net/nodo-local/mtu)" = 1500 ]; then r=0; else r=1; fi
	result "its host name and MTU are ignored" "$r"
	kill "$dhcpd_pid" 2>/dev/null
	dhcpd_pid=
else
	echo "SKIP the lease with a router and DNS needs python3: scripts/dev-vm.sh mirror python3"
fi

run_with MODE=dhcp PARENT=nosuchif
wait_status no-parent 15
result "a missing interface is reported" "$?"
run_with MODE=static PARENT=lnhost ADDRESS=192.168.78.3
wait_status failed 15
result "a static address without a prefix is refused" "$?"
run_with MODE=off
wait_status disabled 15
result "switched off is reported as disabled" "$?"
exit "$fail"
