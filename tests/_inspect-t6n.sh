#!/bin/sh
# Reads the finished T6N image and prints what the card will actually boot.
# IMG picks another T6N image, such as the mainline one.
img=${IMG:-/o/images/nanopc-t6n/monokulo-nanopc-t6n-sd.img}
dd if="$img" bs=1M skip=140 status=none of=/tmp/r.ext4 conv=sparse
show() { debugfs -R "$1" /tmp/r.ext4 2>/dev/null; }
echo "guard lines: $(show 'cat /etc/sv/cuprated/run' | grep -cE 'DATA_MOUNT|lib64')"
show 'ls -l /usr/bin/init' | cut -c1-70
echo "media: $(show 'ls -l /media' | awk '{print $NF}' | tr '\n' ' ')"
echo "firmware: $(show 'ls -l /usr/lib/firmware/rtl_nic' | awk '{print $NF}' | grep -v zst | tr '\n' ' ' | cut -c1-120)"
show 'cat /etc/fstab'
