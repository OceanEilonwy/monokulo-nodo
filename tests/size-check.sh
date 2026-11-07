#!/bin/sh
# Fails when the image grows past its budget, so bloat is caught when it appears.
set -eu
root=$(cd "$(dirname "$0")/.." && pwd)
board=${1:-qemu-virt}
. "$root/boards/$board/board.env"
img=$root/out/images/$board
fail=0
budget() { name=$1; size=$2; max=$3; if [ "$size" -le "$max" ]; then echo "PASS $name ${size}MiB of ${max}MiB"; else echo "FAIL $name ${size}MiB of ${max}MiB"; fail=1; fi; }
budget initramfs "$(( $(stat -c %s "$img/initramfs.img") / 1048576 ))" "$BOARD_MAX_INITRAMFS_MB"
budget kernel "$(( $(stat -c %s "$img/vmlinuz") / 1048576 ))" "$BOARD_MAX_KERNEL_MB"
budget rootfs "$(cat "$img/rootfs-mb.txt")" "$BOARD_MAX_ROOTFS_MB"
exit "$fail"
