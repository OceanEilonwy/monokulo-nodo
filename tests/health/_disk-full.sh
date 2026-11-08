#!/bin/sh
# Runs on the device under test, as root, with nodo-healthd at /usr/local/bin. Puts cuprated's data
# on a small loop filesystem and fills it, first without protection to show the crash loop the
# check must catch, then with nodo-healthd running. Nothing outside the VM is touched.
set -u
img=/var/tmp/nodo-data.img
mnt=/media/monero
log=/var/log/cuprated/current
work=/run/nodo-test
fail=0
result() { if [ "$2" = 0 ]; then echo "PASS $1"; else echo "FAIL $1"; fail=1; fi; }
note() { echo "     $*"; }
avail() { df -B1 --output=avail "$mnt" | tail -n 1 | tr -d ' '; }
# svlogd may rotate during the test, so count across rotated files too.
alllog() { cat /var/log/cuprated/@*.[su] "$log" 2>/dev/null; }
starts() { alllog | grep -c "Using config at"; }
enospc_count() { alllog | grep -c "No space left on device"; }
# 18081 is 0x46A1.
rpc_up() { grep -q ":46A1 00000000:0000 0A" /proc/net/tcp; }
wait_for() { n=0; until eval "$2"; do n=$((n + 1)); [ "$n" -ge "$1" ] && return 1; sleep 1; done; }
# Fills the data disk until at most $1 bytes stay free for an unprivileged writer.
fill_to() {
	want=$1
	i=0
	while :; do
		have=$(avail)
		[ "$have" -le "$want" ] && return 0
		i=$((i + 1))
		fallocate -l $((have - want)) "$mnt/filler.$i" 2>/dev/null ||
			dd if=/dev/zero of="$mnt/filler.$i" bs=4096 count=$(((have - want) / 4096 + 1)) 2>/dev/null
		[ "$i" -gt 50 ] && return 1
	done
}
fill_all() {
	fill_to 0
	dd if=/dev/zero of="$mnt/filler.last" bs=4096 2>/dev/null
	sync
}
unfill() { rm -f "$mnt"/filler.*; sync; }
cleanup() {
	pkill -x nodo-healthd 2>/dev/null
	unfill
	rm -f /etc/sv/cuprated/down /etc/sv/cuprated/conf
	sv -w 120 stop cuprated >/dev/null 2>&1
	if [ -d /sys/fs/cgroup/nodo/cuprated ]; then
		for p in $(cat /sys/fs/cgroup/nodo/cuprated/cgroup.procs); do echo "$p" > /sys/fs/cgroup/cgroup.procs; done
		rmdir /sys/fs/cgroup/nodo/cuprated /sys/fs/cgroup/nodo 2>/dev/null
	fi
	umount "$mnt" 2>/dev/null
	rm -f "$img"
	rm -rf "$work"
	sv -w 120 start cuprated >/dev/null 2>&1
}
trap cleanup EXIT

sv -w 120 stop cuprated >/dev/null
truncate -s 300M "$img"
mkfs.ext4 -q -F -m 0 "$img"
mkdir -p "$mnt" "$work"
mount -o loop "$img" "$mnt" || { echo "FAIL could not mount the test filesystem"; exit 1; }
echo "DATA_MOUNT=$mnt" > /etc/sv/cuprated/conf
sv start cuprated >/dev/null
wait_for 180 rpc_up
result "cuprated runs with its data on the small filesystem" $?
note "free on data disk: $(avail) bytes"

# Control: without protection a full disk must crash-loop cuprated, or this test proves nothing.
# An idle, offline node writes almost nothing, so the control starts it on an empty data
# directory, where it must create its database, as a first start or a resync would.
sv -w 120 stop cuprated >/dev/null
mv "$mnt/cuprated" "$mnt/cuprated.keep"
install -d -o _cuprated -g _cuprated -m 750 "$mnt/cuprated"
fill_all
before=$(starts)
sv start cuprated >/dev/null
sleep 45
loops=$(($(starts) - before))
enospc=$(tail -n 400 "$log" | grep -c "No space left on device")
note "starts in 45s with a full disk: $loops, ENOSPC lines: $enospc"
if [ "$loops" -ge 3 ] && [ "$enospc" -ge 1 ]; then rc=0; else rc=1; fi
result "control: unprotected full disk crash-loops cuprated" "$rc"
tail -n 400 "$log" | grep -m 1 "No space left" | cut -c1-200
sv -w 120 stop cuprated >/dev/null
unfill
rm -rf "$mnt/cuprated"
mv "$mnt/cuprated.keep" "$mnt/cuprated"
sv start cuprated >/dev/null
wait_for 180 rpc_up
result "control: cuprated recovers once space is freed" $?

cat > "$work/health.toml" <<CONF
interval_secs = 1
sv_dir = "/etc/sv"
state_path = "$work/state.toml"
report_path = "$work/health.json"

[[filesystems]]
name = "system"
path = "/"
warn = { bytes = 1048576 }
stop = { bytes = 524288 }
resume_margin = { bytes = 1 }
system = true

[[filesystems]]
name = "data"
path = "$mnt"
warn = { bytes = 83886080 }
stop = { bytes = 41943040 }
resume_margin = { bytes = 31457280 }
services = ["cuprated"]
require_mount = true

[logs]
roots = ["/var/log"]
file_bytes = 1048576
files = 5

[memory]
cgroup_root = "/sys/fs/cgroup"
[memory.limits.cuprated]
user = "_cuprated"
max_bytes = 3221225472
CONF
nohup /usr/local/bin/nodo-healthd --config "$work/health.toml" > "$work/healthd.log" 2>&1 </dev/null &
sleep 3
grep -q '"level": "Ok"' "$work/health.json"
result "health report is written" $?
pid=$(pgrep -x cuprated)
grep -q "/nodo/cuprated" "/proc/$pid/cgroup" && [ "$(cat /sys/fs/cgroup/nodo/cuprated/memory.max)" = 3221225472 ]
result "cuprated is placed in its memory-limited cgroup" $?
if [ "$(cat /var/log/cuprated/config 2>/dev/null)" = "$(printf 's1048576\nn5')" ]; then rc=0; else rc=1; fi
result "svlogd size limits are written" "$rc"
grep -q '"synced": false' "$work/health.json" && chronyc -n tracking | grep -q "Not synchronised"
result "time sync status matches chrony (the VM is offline)" $?

fill_to 62914560
sleep 3
rpc_up && grep -q '"level": "Low"' "$work/health.json" && grep -q '"refuse_installs": false' "$work/health.json"
result "low data disk warns and keeps the node running" $?

before=$(starts)
fill_to 20971520
wait_for 60 '! pgrep -x cuprated >/dev/null'
result "node is stopped before the data disk fills" $?
# The report is written once runit confirms the stop, slightly after the process exits.
sleep 5
tail -n 60 "$log" | grep -q "Shutdown complete"
result "the stop was a clean shutdown" $?
[ -e /etc/sv/cuprated/down ] && grep -q '"service": "cuprated"' "$work/health.json" && grep -q '"level": "Critical"' "$work/health.json"
result "the stop survives reboots and is reported for Home" $?
grep -o '"message": "[^"]*"' "$work/health.json" | head -n 2 | sed 's/^/     /'

e0=$(enospc_count)
fill_all
sleep 40
[ "$(starts)" = "$before" ] && ! pgrep -x cuprated >/dev/null
result "no restart loop while the disk stays full" $?
if [ "$(enospc_count)" = "$e0" ]; then rc=0; else rc=1; fi
result "no new ENOSPC failures while protected" "$rc"

unfill
alllog > "$work/before-recovery.log"
mark=$(($(wc -l < "$work/before-recovery.log") + 1))
wait_for 120 rpc_up
result "node starts again by itself once space is freed" $?
[ ! -e /etc/sv/cuprated/down ] && grep -q '"stopped": \[\]' "$work/health.json"
result "the protection is lifted and the report is clear" $?
alllog | tail -n +"$mark" | grep -iE "panicked|corrupt|No space left" | head -n 3
! alllog | tail -n +"$mark" | grep -qiE "panicked|corrupt"
result "the database opens without errors after recovery" $?
sed 's/^/     healthd: /' "$work/healthd.log" | tail -n 8
exit "$fail"
