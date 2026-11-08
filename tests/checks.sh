# Sourced by the test runners. Expects ssh_vm, check and fail to be defined.
echo "PASS ssh login with key"

check "boot diagnostics are saved" 'for n in $(seq 1 30); do [ -s /var/log/monokulo-diag/latest.txt ] && break; sleep 2; done; grep -q "== services" /var/log/monokulo-diag/latest.txt && grep -q "run: /var/service/cuprated:" /var/log/monokulo-diag/latest.txt'
check "console shows the IP address" 'grep -q "\\\\4" /etc/issue'
check "loopback is up" "ip -o link show lo | grep -q LOWER_UP"
check "password login disabled" 'sshd -T | grep -qix "passwordauthentication no"'
check "root password locked" 'passwd -S root | grep -q "^root L"'
check "firewall default drop" 'nft list chain inet monokulo input | grep -q "policy drop"'
check "cuprated service up" 'for n in $(seq 1 24); do sv status cuprated | grep -q "^run:" && exit 0; sleep 5; done; sv status cuprated; exit 1'
check "cuprated runs as service user" 'ps -o user= -C cuprated | grep -qx _cuprated'
check "cuprated is sandboxed" 'p=$(pgrep -x cuprated); [ -n "$p" ] && [ ! -e /proc/$p/root/root ] && [ ! -e /proc/$p/root/etc/shadow ] && [ "$(readlink /proc/$p/ns/pid)" != "$(readlink /proc/1/ns/pid)" ]'
# The guest has no HTTP client and RPC only listens on localhost, so tunnel to it.
ssh -q -N -L 18081:127.0.0.1:18081 -p 2222 -i /src/build/test-key -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null root@127.0.0.1 &
tunnel=$!
sleep 2
n=0; rpc_ok=0
while [ "$n" -lt 60 ]; do
	if curl -sf -m 5 http://127.0.0.1:18081/get_info > /tmp/get_info.json && grep -q '"status": *"OK"' /tmp/get_info.json; then rpc_ok=1; break; fi
	n=$((n+1)); sleep 5
done
if [ "$rpc_ok" = 1 ]; then echo "PASS rpc answers get_info"; tr -d ' \n' < /tmp/get_info.json | cut -c1-300; echo; else echo "FAIL rpc answers get_info"; fail=1; fi
kill "$tunnel" 2>/dev/null
check "log is not flooded" '! grep -q "Commands:" /var/log/cuprated/current && [ -z "$(ls /var/log/cuprated/*.s 2>/dev/null)" ]'
check "refuses to start without its data drive" 'sv -w 120 stop cuprated >/dev/null; echo DATA_MOUNT=/nonexistent > /etc/sv/cuprated/conf; sv start cuprated; sleep 3; r=1; tail -n 5 /var/log/cuprated/current | grep -q "refusing to start" && ! pgrep -x cuprated >/dev/null && r=0; rm -f /etc/sv/cuprated/conf; sv -w 60 restart cuprated >/dev/null; for n in 1 2 3 4 5 6 7 8 9 10 11 12; do pgrep -x cuprated >/dev/null && break; sleep 5; done; exit $r'
check "stdin prompt is disabled" 'grep -q "disabling STDIN commands" /var/log/cuprated/current'
# The shutdown handler starts with the node, so wait for RPC before testing it.
check "clean stop under TERM" 'for n in $(seq 1 60); do grep -q ":46A1 00000000:0000 0A" /proc/net/tcp && break; sleep 2; done; sv -w 120 stop cuprated | grep -q "^ok: down" && tail -n 40 /var/log/cuprated/current | grep -q "Shutdown complete"'
# The script goes in a file so nothing it runs can consume its own text from stdin.
ssh_vm 'cat > /tmp/fw.sh' < /src/tests/firewall-traffic.sh
ssh_vm 'sh /tmp/fw.sh </dev/null' || fail=1
check "service restarts" 'sv -w 60 start cuprated | grep -q "^ok: run"'
. /src/tests/web-checks.sh
if [ "${SKIP_REBOOT:-}" != 1 ]; then
ssh_vm 'for n in $(seq 1 60); do grep -q ":46A1 00000000:0000 0A" /proc/net/tcp && break; sleep 2; done'
boot1=$(ssh_vm 'cat /proc/sys/kernel/random/boot_id')
reboot_started=$(date +%s)
ssh_vm 'shutdown -r now' || true
sleep 15
i=0
while :; do
	boot2=$(ssh_vm 'cat /proc/sys/kernel/random/boot_id' 2>/dev/null || true)
	[ -n "$boot2" ] && [ "$boot2" != "$boot1" ] && break
	i=$((i+1)); [ "$i" -gt 60 ] && break; sleep 5
done
if [ "$i" -gt 60 ]; then echo "FAIL reboot"; fail=1; else echo "PASS reboot in $(($(date +%s) - reboot_started))s"; fi
check "cuprated closed cleanly at reboot" '[ "$(cat /var/log/cuprated/*.s /var/log/cuprated/current 2>/dev/null | grep -c "Shutdown complete")" -ge 2 ]'
check "cuprated back after reboot" 'for n in $(seq 1 24); do sv status cuprated | grep -q "^run:" && exit 0; sleep 5; done; exit 1'
fi

ssh_vm 'df -h / | tail -1; free -m | sed -n 2p; du -sh /usr /var/lib/cuprated 2>/dev/null; xbps-query -l | wc -l' || true
