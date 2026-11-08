#!/bin/sh
# Boots an image and checks the running system over SSH.
# Usage: tests/qemu-boot-test.sh [board]
set -eu
. "$(dirname "$0")/../scripts/lib.sh"
ensure_builder
board=${1:-qemu-virt}
. "$root/boards/$board/board.env"
# Some boards also run kernel checks that need binaries built on this machine.
[ "${BOARD_KERNEL_CHECKS:-}" != 1 ] || "$root/tests/build-landlock-probe.sh"
set --
[ "${BOARD_NEEDS_KVM:-}" = 1 ] && set -- --device /dev/kvm
exec docker run --rm -e "BOARD=$board" "$@" -v "$root:/src" "$BUILDER" /src/tests/_qemu-boot-inner.sh
