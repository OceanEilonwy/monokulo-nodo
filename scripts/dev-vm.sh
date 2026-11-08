#!/bin/sh
# Fast loop: keep one VM running and push rebuilt packages into it.
# Uses the KVM board by default. Set DEV_BOARD=qemu-virt for emulated aarch64.
# Usage: scripts/dev-vm.sh up | push <package>... | check | mirror <package>... | ssh [command] | down
#        | fresh | usb-create NAME MB | usb-plug NAME [on] | usb-unplug NAME   (on = read-only)
set -eu
. "$(dirname "$0")/lib.sh"
board=${DEV_BOARD:-x86-kvm}
. "$root/boards/$board/board.env"
# Worktrees get separate VMs, so parallel work never shares or stops another VM.
name=monokulo-dev-$board-$(printf %s "$root" | sha256sum | cut -c1-8)
cmd=${1:?usage: dev-vm.sh up|push|check|mirror|ssh|down}
[ $# -gt 0 ] && shift
in_vm() { docker exec -i -e "ARCH=$BOARD_ARCH" "$name" sh -c '. /src/tests/_vm-lib.sh; "$@"' sh "$@"; }
case $cmd in
up)
	ensure_builder
	if docker inspect "$name" >/dev/null 2>&1; then echo "already running"; exit 0; fi
	set --
	[ "${BOARD_NEEDS_KVM:-}" = 1 ] && set -- --device /dev/kvm
	docker run -d --rm --name "$name" -e "BOARD=$board" "$@" -v "$root:/src" "$BUILDER" /src/tests/_vm-serve.sh >/dev/null
	in_vm wait_ssh
	echo "ready: $board"
	;;
push)
	"$root/scripts/build-packages.sh" "$BOARD_ARCH" "$@"
	in_vm push_packages "$@"
	;;
check) docker exec -i -e SKIP_REBOOT=1 "$name" /src/tests/_vm-check.sh ;;
# Test tools from the pinned Void mirror, such as curl and python3.
mirror) in_vm push_mirror_packages "$@" ;;
ssh) in_vm ssh_vm "$@" ;;
# A new VM from the clean image, keeping the container and its virtual USB drives.
fresh) in_vm fresh_vm ;;
# Virtual USB drives: disk image files inside the container, never host devices.
usb-create) docker exec "$name" sh -c 'rm -f "/tmp/$1.img" && truncate -s "${2}M" "/tmp/$1.img"' sh "$@" ;;
usb-plug) in_vm usb_plug "$1" "/tmp/$1.img" "${2:-off}" ;;
usb-unplug) in_vm usb_unplug "$1" ;;
down) docker stop -t 2 "$name" >/dev/null ;;
*) echo "unknown command: $cmd" >&2; exit 2 ;;
esac
