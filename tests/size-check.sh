#!/bin/sh
# Fails when the image grows past its budget, so bloat is caught when it appears. scripts/build-image.sh
# runs it after every build. A budget the board does not set is not checked: the T6N boards have
# no initramfs, and their kernel is the one in the kernel partition (kernel-bytes.txt), not the
# stock kernel that configured the image under QEMU.
set -eu
root=$(cd "$(dirname "$0")/.." && pwd)
board=${1:-qemu-virt}
. "$root/boards/$board/board.env"
img=$root/out/images/$board
fail=0
budget() { name=$1; size=$2; max=$3; if [ "$size" -le "$max" ]; then echo "PASS $name ${size}MiB of ${max}MiB"; else echo "FAIL $name ${size}MiB of ${max}MiB"; fail=1; fi; }
mib() { echo $(( $1 / 1048576 )); }
[ -z "${BOARD_MAX_INITRAMFS_MB:-}" ] || budget initramfs "$(mib "$(stat -c %s "$img/initramfs.img")")" "$BOARD_MAX_INITRAMFS_MB"
if [ -n "${BOARD_MAX_KERNEL_MB:-}" ]; then
	if [ -f "$img/kernel-bytes.txt" ]; then
		budget kernel "$(mib "$(cat "$img/kernel-bytes.txt")")" "$BOARD_MAX_KERNEL_MB"
	else
		budget kernel "$(mib "$(stat -c %s "$img/vmlinuz")")" "$BOARD_MAX_KERNEL_MB"
	fi
fi
budget rootfs "$(cat "$img/rootfs-mb.txt")" "${BOARD_MAX_ROOTFS_MB:?the board sets no root filesystem budget}"
exit "$fail"
