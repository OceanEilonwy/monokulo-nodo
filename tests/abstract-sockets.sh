#!/bin/sh
# Abstract Unix socket listeners, on the device under test, as root, with nodo-tools installed
# and python3 from the mirror (scripts/dev-vm.sh mirror python3) for the throwaway listener.
#
# On a clean system nothing should listen on an abstract socket, since on a kernel without
# Landlock scoping (the T6N's 6.1) a sandboxed service granted a Unix socket could reach it. Then
# an unprivileged user opens one, and nodo-healthd must name it on Home, with its process and
# user, and drop the warning once it closes.
#   scripts/dev-vm.sh ssh 'cat > /tmp/abstract-sockets.sh' < tests/abstract-sockets.sh
#   scripts/dev-vm.sh ssh 'sh /tmp/abstract-sockets.sh </dev/null'
# tests/helper-e2e.sh sources it from /tmp/abstract-sockets.sh when present, using its own
# check and ctl.
set -u
if ! type check >/dev/null 2>&1; then
	as_fail=0
	as_out=/tmp/as.out
	check() { as_name=$1; shift; if ( "$@" ) >"$as_out" 2>&1; then echo "PASS $as_name"; else echo "FAIL $as_name"; as_fail=1; sed 's/^/    /' "$as_out" | head -n 25; fi; }
	mkdir -p /tmp/as-bin
	printf '#!/bin/sh\nexec nodo-tui --ctl "$@"\n' > /tmp/as-bin/ctl
	chmod 755 /tmp/as-bin/ctl
	PATH=/tmp/as-bin:$PATH
	export PATH
	as_standalone=1
else
	as_standalone=
fi

as_helper_link=''
as_healthd_link=''
[ -L /var/service/nodo-helper ] || { ln -s /etc/sv/nodo-helper /var/service/; as_helper_link=1; }
[ -L /var/service/nodo-healthd ] || { ln -s /etc/sv/nodo-healthd /var/service/; as_healthd_link=1; }
for _ in $(seq 1 30); do ctl health 2>/dev/null | grep -q "reporting: true" && break; sleep 1; done

echo "abstract Unix sockets on this system:"
nodo-healthd --abstract-sockets | sed 's/^/    /'
check "the clean system has no unexpected abstract socket listeners" nodo-healthd --abstract-sockets
check "Home has no abstract socket warning on the clean system" sh -c '
	ctl health > /tmp/as.h; grep -q "reporting: true" /tmp/as.h && ! grep -q "abstract Unix socket" /tmp/as.h'

if command -v python3 >/dev/null; then
	# What Home must say about whether this kernel's sandbox can block it.
	if nodo-confine --check | grep -q "abstract_scope=yes"; then
		as_verdict="sandbox keeps services away from it"
	else
		as_verdict="cannot stop sandboxed services that use Unix sockets"
	fi
	chpst -u nobody python3 -c '
import socket, time
s = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
s.bind("\0nodo-e2e-probe\0x")
s.listen(1)
time.sleep(300)
' &
	as_pid=$!
	check "nodo-healthd --abstract-sockets names an unprivileged listener" sh -c '
		for _ in $(seq 1 20); do
			nodo-healthd --abstract-sockets > /tmp/as.l
			[ $? = 1 ] && grep -q "UNEXPECTED @nodo-e2e-probe@x .*python3 (pid [0-9]*) as user nobody" /tmp/as.l && exit 0
			sleep 0.5
		done
		cat /tmp/as.l; exit 1'
	check "Home warns about it, naming the process, the user and what the sandbox can do" sh -c '
		for _ in $(seq 1 40); do
			ctl health > /tmp/as.h
			grep "abstract Unix socket @nodo-e2e-probe@x: python3 (pid [0-9]*) as user nobody" /tmp/as.h | grep -q "$0" && exit 0
			sleep 1
		done
		cat /tmp/as.h; exit 1' "$as_verdict"
	check "the report lists it with its owner" sh -c 'grep -q "\"name\": \"@nodo-e2e-probe@x\"" /run/nodo/health.json'
	kill "$as_pid"
	wait "$as_pid" 2>/dev/null
	check "the warning clears once the listener stops" sh -c '
		for _ in $(seq 1 40); do
			ctl health > /tmp/as.h
			grep -q "reporting: true" /tmp/as.h && ! grep -q "abstract Unix socket" /tmp/as.h && exit 0
			sleep 1
		done
		cat /tmp/as.h; exit 1'
else
	check "python3 is needed for the throwaway listener: scripts/dev-vm.sh mirror python3" false
fi

[ -n "$as_healthd_link" ] && rm -f /var/service/nodo-healthd
[ -n "$as_helper_link" ] && rm -f /var/service/nodo-helper
if [ -n "$as_standalone" ]; then exit "$as_fail"; fi
