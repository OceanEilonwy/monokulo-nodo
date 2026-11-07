#!/bin/sh
# Runs on the device under test. Sends real traffic to a LAN-like peer in a network namespace,
# because a rule can load cleanly and still block the wrong thing. Root is the control, showing
# that a blocked result comes from the firewall and not from routing.
ns=fwpeer
key=/tmp/fwkey
fail=0
cleanup() {
	pkill -u _cuprated -x sshd 2>/dev/null
	ip netns pids "$ns" 2>/dev/null | xargs -r kill 2>/dev/null
	ip link del fwhost 2>/dev/null
	ip netns del "$ns" 2>/dev/null
	rm -f "$key" "$key.pub"
}
trap cleanup EXIT
result() { if [ "$2" = 0 ]; then echo "PASS $1"; else echo "FAIL $1"; fail=1; fi; }
scan() { ssh-keyscan -T 3 -p "$1" "$2" 2>/dev/null | grep -q ssh-ed25519; }

rm -f "$key" "$key.pub"
ssh-keygen -q -t ed25519 -N "" -f "$key" </dev/null
chown _cuprated "$key"
ip netns add "$ns"
ip link add fwhost type veth peer name fwpeer netns "$ns"
ip addr add 192.168.77.2/24 dev fwhost
ip -6 addr add fd77::2/64 dev fwhost nodad
ip link set fwhost up
ip netns exec "$ns" ip link set lo up
ip netns exec "$ns" ip addr add 192.168.77.1/24 dev fwpeer
ip netns exec "$ns" ip -6 addr add fd77::1/64 dev fwpeer nodad
ip netns exec "$ns" ip link set fwpeer up
ip netns exec "$ns" /usr/bin/sshd -f /dev/null -h "$key" -p 2022 -o PidFile=none -o ListenAddress=0.0.0.0 -o ListenAddress=:: -o UsePAM=no

for target in 192.168.77.1 fd77::1; do
	scan 2022 "$target"; c=$?
	result "control: root reaches LAN peer $target" "$c"
	if chpst -u _cuprated sh -c "ssh-keyscan -T 3 -p 2022 $target 2>/dev/null" | grep -q ssh-ed25519; then r=1; else r=0; fi
	result "service user cannot open connections to LAN peer $target" "$r"
done

sv -w 120 stop cuprated >/dev/null
nohup chpst -u _cuprated /usr/bin/sshd -D -e -f /dev/null -h "$key" -p 18080 -o PidFile=none -o ListenAddress=0.0.0.0 -o ListenAddress=:: -o UsePAM=no >/tmp/fw.log 2>&1 </dev/null &
i=0; until grep -q ':46B0 ' /proc/net/tcp || [ "$i" -ge 10 ]; do i=$((i+1)); sleep 1; done
for target in 192.168.77.2 fd77::2; do
	ip netns exec "$ns" ssh-keyscan -T 5 -p 18080 "$target" 2>/dev/null | grep -q ssh-ed25519
	result "service user replies to inbound LAN peer via $target" "$?"
done
tail -n 2 /tmp/fw.log | cut -c1-120 >&2
pkill -u _cuprated -x sshd
rm -f /tmp/fw.log
sv -w 60 start cuprated >/dev/null
exit "$fail"
