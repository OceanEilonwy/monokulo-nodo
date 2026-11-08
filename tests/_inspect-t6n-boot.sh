#!/bin/sh
# Checks the handoff from FriendlyElec's ramdisk to the Void root filesystem.
# IMG picks another T6N image, such as the mainline one.
img=${IMG:-/o/images/nanopc-t6n/monokulo-nanopc-t6n-sd.img}
dd if="$img" bs=1M skip=140 status=none of=/tmp/r.ext4 conv=sparse
show() { debugfs -R "$1" /tmp/r.ext4 2>/dev/null; }
for p in /sbin /usr/bin/init /usr/bin/runit-init /usr/bin/runit /usr/bin/sh /bin; do
	printf '%s: ' "$p"
	show "stat $p" | grep -E 'Type:|Fast link dest' | tr -s ' ' | cut -c1-70 | tr '\n' ' '
	echo
done
# boot.img is the vendor ramdisk: find and unpack it.
python3 -I - "$1" <<'PY'
import sys,gzip,lzma,zlib
d=open(sys.argv[1],'rb').read()
print('size',len(d),'head',d[:16])
for name,magic in (('gzip',b'\x1f\x8b\x08'),('xz',b'\xfd7zXZ'),('lz4',b'\x02!L\x18'),('cpio',b'070701')):
    print(name,d.find(magic))
i=d.find(b'\x1f\x8b\x08')
if i>=0:
    raw=zlib.decompressobj(31).decompress(d[i:])
    open('/tmp/rd.cpio','wb').write(raw); print('ramdisk',len(raw))
PY
mkdir -p /tmp/rd && cd /tmp/rd && cpio -idm --quiet < /tmp/rd.cpio 2>/dev/null
ls /tmp/rd | tr '\n' ' '; echo
echo '=== init'; sed -n 1,200p /tmp/rd/init | grep -nE 'root=|root\b|switch_root|/sbin/init|init=|data=|overlay|mmcblk|fsck|resize|rootfstype|exec ' | cut -c1-120 | head -40
