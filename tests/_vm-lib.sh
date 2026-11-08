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
# A command to the QEMU monitor, which plugs virtual USB drives in and out.
monitor() { bash -c 'exec 3<>/dev/tcp/127.0.0.1/4444 && printf "%s\n" "$1" >&3 && sleep 1 && timeout 1 cat <&3 | tr -d "\r" | grep -av "^QEMU\|^(qemu)" | grep -a . ; true' sh "$*"; }
# Plugs in a disk image from the container as a removable USB drive. `ro` makes it read-only, as a
# write-protect switch does.
usb_plug() {
	id=$1; file=$2; ro=${3:-off}
	monitor "drive_add 0 if=none,id=$id-drive,file=$file,format=raw,readonly=$ro"
	monitor "device_add usb-storage,id=$id,drive=$id-drive,removable=on,bus=xhci.0"
}
usb_unplug() { monitor "device_del $1"; }
# Stops QEMU so that _vm-serve.sh boots a new VM from the clean image.
fresh_vm() { touch /tmp/vm-fresh && pkill -f '^qemu-system' && sleep 5 && wait_ssh; }
# Installs packages from the pinned Void mirror into the VM, which is offline. Only for test tools.
push_mirror_packages() {
	. /src/versions.env
	case $ARCH in aarch64) repo=$VOID_MIRROR/current/aarch64 ;; *) repo=$VOID_MIRROR/current ;; esac
	dl=/tmp/mirror-$ARCH
	mkdir -p "$dl/root/var/db/xbps/keys" "$dl/cache"
	cp /var/db/xbps/keys/* "$dl/root/var/db/xbps/keys/"
	XBPS_ARCH=$ARCH xbps-install -S -y -D -r "$dl/root" -c "$dl/cache" -R "$repo" "$@" >/dev/null || return 1
	ssh_vm 'rm -rf /tmp/mirror && mkdir -p /tmp/mirror'
	tar -C "$dl/cache" -cf - $(cd "$dl/cache" && ls ./*.xbps) | ssh_vm 'tar -C /tmp/mirror -xf -'
	ssh_vm 'xbps-rindex -a /tmp/mirror/*.xbps >/dev/null && for p in '"$*"'; do xbps-query "$p" >/dev/null || xbps-install -y -R /tmp/mirror "$p" >/dev/null || exit 1; done'
}
