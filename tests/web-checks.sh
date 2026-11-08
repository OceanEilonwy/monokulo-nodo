# Sourced by checks.sh in the dev VM container. Runs the web proxy tests when nodo-caddy has been built
# for this board, so boards and tiers without it are unaffected.
. "/src/boards/$BOARD/board.env"
ARCH=$BOARD_ARCH
if command -v push_packages >/dev/null && ls "/src/out/feed/$ARCH"/nodo-caddy-[0-9]*.xbps >/dev/null 2>&1; then
	if push_packages nodo-caddy >/dev/null && push_mirror_packages curl; then
		echo "PASS web test packages installed"
		ssh_vm 'rm -rf /tmp/web && mkdir -p /tmp/web'
		tar -C /src/crates/nodo-web/tests/fixtures/vm -cf - . | ssh_vm 'tar -C /tmp/web -xf -'
		ssh_vm 'cat > /tmp/web-routing.sh' < /src/tests/web-routing.sh
		ssh_vm 'sh /tmp/web-routing.sh </dev/null' || fail=1
	else
		echo "FAIL web test packages installed"; fail=1
	fi
fi
