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

run_with MODE=dhcp PARENT=eth0 LEASE_WAIT=30
wait_status ready 60
result "macvlan gets its own lease" "$?"
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
