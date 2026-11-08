#!/bin/sh
# Runs inside the disposable build container. Packages are configured by booting
# the target root filesystem once in emulation, so no host privileges are needed.
set -eu
. "/src/boards/$BOARD/board.env"
out="/src/out/images/$BOARD"
trap 'chown -R "$HOST_UID:$HOST_GID" /src/out' EXIT

case $BOARD_ARCH in
aarch64) repo=$VOID_MIRROR/current/aarch64 ;;
*) repo=$VOID_MIRROR/current ;;
esac
cache=/src/build/xbps-cache
mkdir -p "$cache"
keys=/var/db/xbps/keys
rootfs=/work/rootfs
mkdir -p "$rootfs$keys" /work/local /work/stage1$keys "$rootfs/etc/xbps.d"
# Boards list only the firmware they need, which keeps the image small.
# nftables only needs Python for its optional bindings, which nothing here uses.
for f in python3 ${BOARD_FIRMWARE_IGNORE:-}; do echo "ignorepkg=$f"; done > "$rootfs/etc/xbps.d/10-monokulo-firmware.conf"
# Skip files a headless appliance never reads. This also applies to later updates.
# Patterns are for xbps, so the shell must not expand them against this container.
set -f
for f in /usr/share/man/* /usr/share/doc/* /usr/share/info/* /usr/share/locale/* /usr/share/i18n/* /boot/System.map-* /boot/config-* ${BOARD_NOEXTRACT:-}; do
	echo "noextract=$f"
done > "$rootfs/etc/xbps.d/20-monokulo-noextract.conf"
set +f
cp "$keys"/* "$rootfs$keys/"
cp "$keys"/* "/work/stage1$keys/"

XBPS_TARGET_ARCH=$BOARD_ARCH xbps-install -SyU -c "$cache" -r "$rootfs" -R "${NODO_REPO:-/src/out/feed/$BOARD_ARCH}" -R "$repo" \
	monokulo-base cuprated-service ${BOARD_PACKAGES:-linux dracut zstd} >/dev/null

# Offline configuration.
mkdir -p "$rootfs/etc/dracut.conf.d"
cat > "$rootfs/etc/dracut.conf.d/monokulo.conf" <<CONF
hostonly=no
dracutmodules=" base rootfs-block kernel-modules udev-rules fs-lib "
drivers=" $BOARD_DRACUT_DRIVERS "
filesystems=" ext4 "
compress="zstd -19"
do_strip=yes
CONF
echo monokulo-nodo > "$rootfs/etc/hostname"
printf '%s / ext4 defaults,noatime 0 1\n' "$BOARD_ROOT" > "$rootfs/etc/fstab"
if [ -f /keys ]; then
	install -d -m 700 "$rootfs/root/.ssh"
	install -m 600 /keys "$rootfs/root/.ssh/authorized_keys"
fi
sv="$rootfs/etc/runit/runsvdir/default"
mkdir -p "$sv"
for s in sshd dhcpcd chronyd nftables cuprated monokulo-diag ${BOARD_SERVICES:-}; do
	[ -d "$rootfs/etc/sv/$s" ] || { echo "missing service: $s" >&2; exit 1; }
	ln -sfn "/etc/sv/$s" "$sv/$s"
done
if [ -d "$rootfs/etc/sv/agetty-$BOARD_CONSOLE" ]; then
	ln -sfn "/etc/sv/agetty-$BOARD_CONSOLE" "$sv/agetty-$BOARD_CONSOLE"
fi

[ -z "${BOARD_FSTAB_EXTRA:-}" ] || printf '%s\n' "$BOARD_FSTAB_EXTRA" >> "$rootfs/etc/fstab"
# base-files owns /etc/issue, so the console banner showing the IP is copied in here.
cp "$rootfs/usr/share/monokulo/issue" "$rootfs/etc/issue"
# dhcpcd owns /etc/dhcpcd.conf, so nodo-localnet's interface block is appended here too.
cat "$rootfs/usr/share/monokulo/dhcpcd-nodo-local.conf" >> "$rootfs/etc/dhcpcd.conf"
for d in ${BOARD_MKDIRS:-}; do mkdir -p "$rootfs$d"; done
if [ -n "${BOARD_DECOMPRESS_FIRMWARE:-}" ]; then
	find "$rootfs/usr/lib/firmware" -name '*.zst' -exec zstd -dqk {} +
fi
[ ! -d "/src/boards/$BOARD/overlay" ] || cp -a "/src/boards/$BOARD/overlay/." "$rootfs/"

cat > "$rootfs/monokulo-configure" <<'CONF'
#!/bin/sh
export PATH=/usr/bin:/usr/sbin
mkdir -p /proc /sys /dev /tmp /run /var/tmp
chmod 1777 /tmp /var/tmp
mount -t proc proc /proc
mount -t sysfs sys /sys
mount -t devtmpfs dev /dev
mount -t tmpfs tmp /tmp
mount -t tmpfs run /run
rc=0
xbps-reconfigure -fa || rc=1
kver=$(ls /usr/lib/modules | sort -V | tail -1)
# Some modules are not extracted, so the module index must match what is present.
depmod "$kver" || rc=1
rm -f /boot/initramfs-*.img
command -v dracut >/dev/null || NO_DRACUT=1
[ -n "${NO_DRACUT:-}" ] || [ -s "/boot/initramfs-$kver.img" ] || dracut --force --no-hostonly --kver "$kver" "/boot/initramfs-$kver.img" || rc=1
passwd -l root || rc=1
id _cuprated || rc=1
echo "$rc" > /monokulo-configure.rc
umount /run /tmp /dev /sys /proc
CONF
chmod 755 "$rootfs/monokulo-configure"

stagek=$rootfs
if [ -n "${BOARD_STAGE_KERNEL:-}" ]; then
	stagek=/work/stagek
	mkdir -p "$stagek$keys"
	cp "$keys"/* "$stagek$keys/"
	XBPS_TARGET_ARCH=$BOARD_ARCH xbps-install -SyU -c "$cache" -r "$stagek" -R "$repo" "$BOARD_STAGE_KERNEL" >/dev/null
fi
kernel=$(find "$stagek/boot" -maxdepth 1 -type f -name 'vmlinu*' | sort -V | tail -1)
[ -n "$kernel" ] || { echo "no kernel found in /boot" >&2; ls -la "$rootfs/boot" >&2; exit 1; }
# Minimal stage that mounts the image and runs the configure script.
kver=$(ls "$stagek/usr/lib/modules" | sort -V | tail -1)
XBPS_TARGET_ARCH=$BOARD_ARCH xbps-install -SyU -c "$cache" -r /work/stage1 -R "$repo" busybox-static >/dev/null
init=/work/initrd
mkdir -p "$init/bin" "$init/mnt" "$init/dev" "$init/proc" "$init/sys"
cp "$(find /work/stage1/usr/bin -name 'busybox*' -type f | head -1)" "$init/bin/busybox"
# Load dependencies first. Numbered names make the init script's glob run in order.
moddir=$stagek/usr/lib/modules/$kver
n=100
for m in $BOARD_STAGE1_MODULES; do
	line=$(grep -E "(^|/)$m\.ko[^:]*:" "$moddir/modules.dep" | head -1)
	if [ -z "$line" ]; then
		grep -qE "/$m\.ko" "$moddir/modules.builtin" || { echo "module not found: $m" >&2; exit 1; }
		continue
	fi
	path=${line%%:*}
	deps=${line#*:}
	for f in $(printf '%s\n' $deps | tac) "$path"; do
		name=$(basename "$f"); name=${name%%.ko*}
		ls "$init"/*-"$name".ko >/dev/null 2>&1 && continue
		n=$((n+1))
		case "$f" in
		*.zst) zstd -dqc "$moddir/$f" > "$init/$n-$name.ko" ;;
		*.xz) xz -dc "$moddir/$f" > "$init/$n-$name.ko" ;;
		*.gz) gzip -dc "$moddir/$f" > "$init/$n-$name.ko" ;;
		*) cp "$moddir/$f" "$init/$n-$name.ko" ;;
		esac
	done
done
cat > "$init/init" <<'INIT'
#!/bin/busybox sh
/bin/busybox --install -s /bin
mount -t proc proc /proc
mount -t devtmpfs dev /dev
for m in /*.ko; do insmod "$m"; done
i=0; until [ -b /dev/vda ] || [ "$i" -ge 30 ]; do sleep 1; i=$((i+1)); done
mount -o rw /dev/vda /mnt && chroot /mnt /monokulo-configure
umount /mnt
sync
echo b > /proc/sysrq-trigger
sleep 30
INIT
chmod 755 "$init/init"
(cd "$init" && find . | cpio -o -H newc --quiet | gzip -1) > /work/stage1.img

truncate -s 3G /work/root.ext4
mke2fs -q -t ext4 -d "$rootfs" -L monokulo-root /work/root.ext4

echo "configuring image under emulation"
timeout 1800 $BOARD_QEMU -smp 8 -m 4096 -nographic -no-reboot \
	-kernel "$kernel" -initrd /work/stage1.img \
	-append "console=${BOARD_STAGE_CONSOLE:-$BOARD_CONSOLE} rdinit=/init panic=1" \
	-drive file=/work/root.ext4,if=virtio,format=raw \
	> "$out/configure.log" 2>&1 || { tail -30 "$out/configure.log"; exit 1; }

rc=$(debugfs -R 'cat /monokulo-configure.rc' /work/root.ext4 2>/dev/null)
[ "$rc" = 0 ] || { echo "image configuration failed, rc='$rc'" >&2; tail -40 "$out/configure.log"; exit 1; }
debugfs -w -R 'rm /monokulo-configure' /work/root.ext4 >/dev/null 2>&1
debugfs -w -R 'rm /monokulo-configure.rc' /work/root.ext4 >/dev/null 2>&1

cp "$kernel" "$out/vmlinuz"
used_mb=$(debugfs -R stats /work/root.ext4 2>/dev/null | awk -F: '/^Block count/{b=$2} /^Free blocks/{f=$2} /^Block size/{s=$2} END{print int((b-f)*s/1048576)}')
echo "$used_mb" > "$out/rootfs-mb.txt"

# Boards with their own boot layout assemble the final image themselves.
if [ -f "/src/boards/$BOARD/assemble.sh" ]; then
	. "/src/boards/$BOARD/assemble.sh"
	ls -lhs "$out"
	exit 0
fi

kver=$(debugfs -R 'ls -l /usr/lib/modules' /work/root.ext4 2>/dev/null | awk '$NF ~ /^[0-9]/{print $NF}' | sort -V | tail -1)
debugfs -R "dump /boot/initramfs-$kver.img $out/initramfs.img" /work/root.ext4 >/dev/null 2>&1
[ -s "$out/initramfs.img" ] || { echo "no initramfs was generated" >&2; exit 1; }
cp "$kernel" "$out/vmlinuz"

truncate -s 3100M "$out/disk.img"
sfdisk -q "$out/disk.img" <<PART
label: gpt
start=2048, size=$(($(stat -c %s /work/root.ext4) / 512)), type=linux
PART
dd if=/work/root.ext4 of="$out/disk.img" bs=1M seek=1 conv=notrunc,sparse status=none
printf 'console=%s root=%s rw\n' "$BOARD_CONSOLE" "$BOARD_ROOT" > "$out/cmdline.txt"
ls -lhs "$out"
