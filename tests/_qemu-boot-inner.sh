#!/bin/sh
# Runs inside a disposable container. The disk copy keeps test runs independent.
set -u
log=/src/out/images/$BOARD/serial.log
. /src/tests/_qemu-lib.sh
(qemu_exec) >"$log" 2>&1 &
qpid=$!
ssh_vm() { ssh -q -p 2222 -i /src/build/test-key -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null -o ConnectTimeout=5 root@127.0.0.1 "$@"; }
fail=0
check() { name=$1; shift; if ssh_vm "$@"; then echo "PASS $name"; else echo "FAIL $name"; fail=1; fi; }

i=0
until ssh_vm true 2>/dev/null; do
	i=$((i+1))
	kill -0 "$qpid" 2>/dev/null || { echo "FAIL qemu exited"; tail -30 "$log"; exit 1; }
	[ "$i" -gt 90 ] && { echo "FAIL ssh never came up"; tail -40 "$log"; kill "$qpid"; exit 1; }
	sleep 5
done
. /src/tests/checks.sh
kill "$qpid" 2>/dev/null
exit "$fail"
