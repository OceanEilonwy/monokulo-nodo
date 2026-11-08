# Sourced by checks.sh in the dev VM container. Runs the system snapshot update test when
# tests/snapshot-prepare.sh has put test snapshots in the VM, so boards and runs without them are
# unaffected. This project's packages are pushed first, so the VM holds exactly the versions the
# snapshots carry, and the helper restarts on the pushed nodo-tools.
. "/src/boards/$BOARD/board.env"
ARCH=$BOARD_ARCH
if command -v push_packages >/dev/null && ssh_vm '[ -s /srv/snapshot-test/server/snapshots/test-d/manifest.toml.sig ]'; then
	if push_packages nodo-tools nodo-caddy nodo-localnet nodo-ddns >/dev/null &&
		ssh_vm '[ ! -L /var/service/nodo-helper ] || sv -w 30 restart nodo-helper >/dev/null'; then
		echo "PASS snapshot test packages installed"
		ssh_vm 'cat > /tmp/snapshot-update.sh' < /src/tests/snapshot-update.sh
		ssh_vm 'sh /tmp/snapshot-update.sh </dev/null' || fail=1
	else
		echo "FAIL snapshot test packages installed"; fail=1
	fi
fi
