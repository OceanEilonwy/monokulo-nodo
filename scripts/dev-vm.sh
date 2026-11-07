#!/bin/sh
# Fast loop: keep one VM running and push rebuilt packages into it.
# Uses the KVM board by default. Set DEV_BOARD=qemu-virt for emulated aarch64.
# Usage: scripts/dev-vm.sh up | push <package>... | check | ssh [command] | down
set -eu
. "$(dirname "$0")/lib.sh"
board=${DEV_BOARD:-x86-kvm}
. "$root/boards/$board/board.env"
name=monokulo-dev-$board
cmd=${1:?usage: dev-vm.sh up|push|check|ssh|down}
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
ssh) in_vm ssh_vm "$@" ;;
down) docker stop -t 2 "$name" >/dev/null ;;
*) echo "unknown command: $cmd" >&2; exit 2 ;;
esac
