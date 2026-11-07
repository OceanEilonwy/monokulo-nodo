#!/bin/sh
# Boots an image and checks the running system over SSH.
# Usage: tests/qemu-boot-test.sh [board]
set -eu
. "$(dirname "$0")/../scripts/lib.sh"
ensure_builder
board=${1:-qemu-virt}
. "$root/boards/$board/board.env"
set --
[ "${BOARD_NEEDS_KVM:-}" = 1 ] && set -- --device /dev/kvm
exec docker run --rm -e "BOARD=$board" "$@" -v "$root:/src" "$BUILDER" /src/tests/_qemu-boot-inner.sh
