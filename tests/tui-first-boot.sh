#!/bin/sh
# Drives the real nodo-tui through first boot in the dev VM, under a pseudo-terminal, against the
# real helper. Needs a fresh VM with nodo-tools installed and the helper enabled:
#   scripts/dev-vm.sh ssh 'cat > /tmp/tui.sh' < tests/tui-first-boot.sh
#   scripts/dev-vm.sh ssh 'sh /tmp/tui.sh </dev/null'
set -u
fail=0
result() { if [ "$1" = 0 ]; then echo "PASS $2"; else echo "FAIL $2"; fail=1; fi; }
rm -f /tmp/tui.out
# Keys arrive with pauses, since the TUI answers them through its worker thread.
(
	sleep 3; printf 'correct horse battery\r'; sleep 1
	printf 'correct horse battery\r'; sleep 6
	printf '\033'; sleep 3
	printf '\r'; sleep 5
	printf '\033'; sleep 2
	printf 'q'; sleep 1
) | script -qfec 'stty rows 32 cols 100; TERM=xterm-256color nodo-tui' /tmp/tui.out >/dev/null 2>&1
# Escape sequences are dropped so the screen text can be searched. Words placed with cursor
# moves run together, so the searches below avoid spaces where that happens.
sed 's/\x1b\[[0-9;?]*[A-Za-z]//g; s/\x1b[()][A-Z0-9]//g' /tmp/tui.out > /tmp/tui.txt
for want in "Set up this device" "Device passphrase" "monokulo-test" "Outbound peers" "Accept inbound peers"; do
	grep -q "$want" /tmp/tui.txt
	result $? "the screen showed: $want"
done
! grep -q "correct horse" /tmp/tui.txt
result $? "the passphrase never appeared on screen"
[ -s /var/lib/nodo/keys/device.key ]
result $? "first boot created the device key"
nodo-tui --ctl overview | grep -q 'encryption: Unlocked'
result $? "the helper reports encryption as set up"
grep -q 'settings-form: ok' /var/log/nodo-helper/current
result $? "the settings form came from the helper"
exit "$fail"
