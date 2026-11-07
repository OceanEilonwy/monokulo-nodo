# Helpers for talking to the VM from inside its container.
ssh_vm() { ssh -q -p 2222 -i /src/build/test-key -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null -o ConnectTimeout=5 root@127.0.0.1 "$@"; }
wait_ssh() { i=0; until ssh_vm true 2>/dev/null; do i=$((i+1)); [ "$i" -gt 90 ] && { echo "ssh never came up" >&2; return 1; }; sleep 2; done; }
push_packages() {
	ssh_vm 'rm -rf /tmp/feed && mkdir -p /tmp/feed'
	for p in "$@"; do
		tar -C "/src/out/feed/$ARCH" -cf - $(cd "/src/out/feed/$ARCH" && ls "$p"-[0-9]*.xbps) | ssh_vm 'tar -C /tmp/feed -xf -'
	done
	ssh_vm 'xbps-rindex -a /tmp/feed/*.xbps >/dev/null && xbps-install -yf -R /tmp/feed '"$*"
	for p in "$@"; do
		case $p in *-service) s=${p%-service}; ssh_vm "sv -w 120 restart $s" ;; esac
	done
}
