# Sourced by the image builder. Uses FriendlyElec's layout in its "plain" mode, where
# the bootloader mounts partition 8 of the SD card as root with no overlay.
sd=/work/sd-fuse
git clone -q --filter=blob:none https://github.com/friendlyarm/sd-fuse_rk3588.git -b kernel-6.1.y "$sd"
git -C "$sd" checkout -q "$BOARD_SDFUSE_COMMIT"
pre=$sd/prebuilt
boot=$rootfs/usr/share/monokulo/boot
[ -s "$boot/kernel.img" ] && [ -s "$boot/resource.img" ] || { echo "kernel package is missing its boot images" >&2; exit 1; }
# tests/size-check.sh holds the kernel the card boots to the board's budget.
stat -c %s "$boot/kernel.img" > "$out/kernel-bytes.txt"

root_sectors=$(($(stat -c %s /work/root.ext4) / 512))
rootfs_start=$((0x46000))
img=$out/monokulo-nanopc-t6n-sd.img
rm -f "$img"
truncate -s $(((rootfs_start + root_sectors + 34) * 512)) "$img"
sfdisk -q "$img" <<PART
label: gpt
first-lba: 34
start=$((0x4000)), size=$((0x2000)), name=uboot
start=$((0x6000)), size=$((0x2000)), name=misc
start=$((0x8000)), size=$((0x2000)), name=dtbo
start=$((0xa000)), size=$((0x8000)), name=resource
start=$((0x12000)), size=$((0x14000)), name=kernel
start=$((0x26000)), size=$((0x10000)), name=boot
start=$((0x36000)), size=$((0x10000)), name=recovery
start=$rootfs_start, size=$root_sectors, name=rootfs
PART

put() {
	max=$3
	size=$(stat -c %s "$1")
	[ $(((size + 511) / 512)) -le "$max" ] || { echo "$1 does not fit its partition" >&2; exit 1; }
	dd if="$1" of="$img" bs=512 seek=$(($2)) conv=notrunc status=none
}
put "$pre/idbloader.img" 0x40 $((0x4000 - 0x40))
put "$pre/uboot.img" 0x4000 $((0x2000))
put "$pre/misc.img" 0x6000 $((0x2000))
put "$pre/dtbo-plain.img" 0x8000 $((0x2000))
put "$boot/resource.img" 0xa000 $((0x8000))
put "$boot/kernel.img" 0x12000 $((0x14000))
put "$pre/boot.img" 0x26000 $((0x10000))
dd if=/work/root.ext4 of="$img" bs=1M seek=$((rootfs_start * 512 / 1048576)) conv=notrunc,sparse status=none
sha256sum "$img" | tee "$img.sha256"
