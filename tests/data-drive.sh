#!/bin/sh
# Runs on the device under test with nodo-tools installed. A virtual data drive inside the VM: a
# sparse file on a loop device, with a GPT partition named NODO_BLOCKCHAIN that /etc/fstab mounts
# by that name, as the T6N images do, and no nodo-helper conf. Checks that the helper finds the
# drive without udev's /dev/disk links, that the health daemon then guards the drive instead of
# counting the node against the system disk, that a full drive stops the node and an emptied one
# starts it again, and that a drive missing at start is still guarded. No real block device is used.
set -u
fail=0
result() { if [ "$2" = 0 ]; then echo "PASS $1"; else echo "FAIL $1"; fail=1; fi; }
mnt=/media/nodo-dd-test
img=/var/tmp/nodo-dd-test.img
conf=/etc/sv/nodo-helper/conf
ctl() { nodo-tui --ctl "$@"; }
wait_socket() { for _ in $(seq 1 30); do [ -S /run/nodo/helper.sock ] && ctl services >/dev/null 2>&1 && return 0; sleep 1; done; return 1; }
restart_helper() { sv -w 30 restart nodo-helper >/dev/null && sleep 1 && wait_socket; }
helper_args() {
	pid=$(sv status nodo-helper | sed -n 's/^run: [^ ]* (pid \([0-9]*\)).*/\1/p')
	[ -n "$pid" ] && tr '\0' ' ' < "/proc/$pid/cmdline"
}
# A filesystem's table in the health configuration, with the services it guards.
fs_block() { awk -v p="path = \"$1\"" '/^\[\[filesystems\]\]/ { i = ++n } /^\[/ && !/^\[\[?filesystems[].]/ { i = 0 }
	i { b[i] = b[i] $0 "\n"; if ($0 == p) k = i } END { if (k) printf "%s", b[k] }' /etc/nodo/health.toml; }
data_section() { fs_block "$mnt"; }
system_section() { fs_block /; }
cleanup() {
	rm -f "$mnt/fill"
	mountpoint -q "$mnt" && umount "$mnt"
	[ -n "${loop:-}" ] && losetup -d "$loop" 2>/dev/null
	rm -f "$img"
	sed -i "\\|^PARTLABEL=NODO_BLOCKCHAIN $mnt |d" /etc/fstab
	rmdir "$mnt" 2>/dev/null
	rm -f "$conf"
	[ -f /tmp/dd-helper.conf ] && mv /tmp/dd-helper.conf "$conf"
	# The helper writes the health configuration without the drive, and the daemon starts
	# again whatever it stopped.
	restart_helper
	for _ in $(seq 1 30); do [ -e /etc/sv/cuprated/down ] || break; sleep 2; done
	[ -n "${healthd_link:-}" ] && rm -f /var/service/nodo-healthd
	[ -n "${helper_link:-}" ] && rm -f /var/service/nodo-helper
	if [ -e /etc/sv/cuprated/down ]; then rm -f /etc/sv/cuprated/down; sv up cuprated; fi
}
command -v losetup >/dev/null && command -v sfdisk >/dev/null && command -v mkfs.xfs >/dev/null && losetup -f >/dev/null 2>&1 ||
	{ echo "SKIP data drive: this system has no loop devices, sfdisk or mkfs.xfs"; exit 0; }
grep -q 'NODO_BLOCKCHAIN' /etc/fstab && { echo "FAIL this device already has a NODO_BLOCKCHAIN drive in fstab"; exit 1; }
trap cleanup EXIT
[ -f "$conf" ] && mv "$conf" /tmp/dd-helper.conf
[ -L /var/service/nodo-helper ] || { ln -s /etc/sv/nodo-helper /var/service/; helper_link=1; }
[ -L /var/service/nodo-healthd ] || { ln -s /etc/sv/nodo-healthd /var/service/; healthd_link=1; }
# runsvdir looks for new services every five seconds.
for _ in $(seq 1 15); do sv status nodo-helper >/dev/null 2>&1 && sv status nodo-healthd >/dev/null 2>&1 && break; sleep 1; done

# Sparse, so it takes no room in the VM: 8 GiB is below the data drive's 20 GiB warning level and
# above its 4 GiB stop level plus the 2 GiB resume margin.
rm -f "$img"
truncate -s 8G "$img"
printf 'label: gpt\nstart=2048, name=NODO_BLOCKCHAIN\n' | sfdisk -q "$img"
loop=$(losetup -P -f --show "$img")
part=${loop}p1
for _ in $(seq 1 10); do [ -b "$part" ] && break; sleep 1; done
mkfs.xfs -q -f "$part"
mkdir -p "$mnt"
echo "PARTLABEL=NODO_BLOCKCHAIN $mnt xfs defaults,noatime,nofail 0 0" >> /etc/fstab
blkid -p -o value -s PART_ENTRY_NAME "$part" | grep -qx NODO_BLOCKCHAIN
result "blkid reads the partition's name from the device itself" "$?"

# Present but not mounted: blkid names the device.
restart_helper
args=$(helper_args)
echo "    helper: $args"
case $args in *"--drive $part --drive-mount $mnt"*) r=0 ;; *) r=1 ;; esac
result "with no conf, the helper takes the drive from fstab and finds its partition" "$r"
grep -q "nodo-helper: data drive $part at $mnt, from /etc/fstab" /var/log/nodo-helper/current
result "the helper's log says which drive it uses" "$?"
data_section | grep -q 'cuprated' && ! system_section | grep -q 'cuprated'
result "the health daemon guards the node on the data drive, not on the system disk" "$?"
ctl drive | grep -q 'present: true' && ctl drive | grep -q 'mounted: false'
result "the drive is reported present and not mounted" "$?"

# Mounted by its partition name through fstab, as at boot.
mount "$mnt" && mountpoint -q "$mnt"
result "fstab mounts the drive by its partition name" "$?"
restart_helper
case $(helper_args) in *"--drive $part --drive-mount $mnt"*) r=0 ;; *) r=1 ;; esac
result "a mounted drive is the device mounted there" "$r"
health_level() { ctl health | grep -A6 "path: \"$mnt\"" | sed -n 's/.*level: "\([A-Za-z]*\)".*/\1/p' | head -n 1; }
for _ in $(seq 1 30); do [ "$(health_level)" = Low ] && break; sleep 2; done
[ "$(health_level)" = Low ] && sv status cuprated | grep -q '^run:'
result "the health daemon measures the data drive, low but not full, and the node runs" "$?"

# Full: below the stop level the node is stopped before a write can fail.
fallocate -l 5G "$mnt/fill"
for _ in $(seq 1 90); do [ -e /etc/sv/cuprated/down ] && ! sv status cuprated | grep -q '^run:' && break; sleep 2; done
ctl health > /tmp/dd-health.txt
echo "    data drive: $(df -h "$mnt" | tail -n 1)"
[ "$(health_level)" = Critical ] && [ -e /etc/sv/cuprated/down ] && ! sv status cuprated | grep -q '^run:'
result "a full data drive is detected and the node is stopped" "$?"
grep -q 'cuprated' /tmp/dd-health.txt && grep -qi 'stopped' /tmp/dd-health.txt
result "Home is told the node was stopped" "$?"
rm -f "$mnt/fill"
for _ in $(seq 1 60); do [ ! -e /etc/sv/cuprated/down ] && sv status cuprated | grep -q '^run:' && break; sleep 2; done
[ ! -e /etc/sv/cuprated/down ] && sv status cuprated | grep -q '^run:'
result "with room again the node starts again" "$?"

# Missing at start: the mount point is still guarded, and reported as not mounted.
umount "$mnt"
losetup -d "$loop"
loop=
restart_helper
case $(helper_args) in *"--drive-mount $mnt"*) r=0 ;; *) r=1 ;; esac
result "a missing drive keeps its mount point" "$r"
data_section | grep -q 'cuprated'
result "and the node stays guarded on it" "$?"
for _ in $(seq 1 15); do ctl health | grep -A3 "path: \"$mnt\"" | grep -q 'mounted: false' && break; sleep 2; done
ctl health | grep -A3 "path: \"$mnt\"" | grep -q 'mounted: false' && ctl drive | grep -q 'present: false'
result "the missing drive is reported as not mounted and not present" "$?"
exit "$fail"
