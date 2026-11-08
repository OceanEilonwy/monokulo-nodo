#!/bin/sh
# USB backups and a restore onto a brand-new device, in the dev VM. Runs on the host, because it
# plugs virtual USB drives in and out through the QEMU monitor and replaces the VM part-way.
# Every drive is a disk image file inside the VM's container; no host device is ever touched.
#
# It starts from a fresh VM of the current image, so run it after the other VM tests:
#   scripts/dev-vm.sh up && tests/usb-e2e.sh
#
# First VM: set up a device with a feed, an encrypted package with a secret setting and a public
# site, Caddy storage, and back it up to a FAT32 USB stick. Refusals for the system disk, the data drive,
# a read-only stick, a full stick and a stick pulled out mid-write are checked on the way.
# Second VM: a clean image with the same stick, not set up, restored from it. The restore puts
# the public site into Caddy's configuration at once, not at the proxy's next reload.
# Public sites ask Pebble's address (/etc/nodo/acme-test.toml), never Let's Encrypt; the VM is
# offline either way.
set -u
root=$(cd "$(dirname "$0")/.." && pwd)
vm() { "$root/scripts/dev-vm.sh" "$@"; }
v() { vm ssh "$@"; }
fail=0
out=$root/build/usb-e2e.out
pass() { echo "PASS $1"; }
bad() { echo "FAIL $1"; fail=1; sed 's/^/    /' "$out" | head -n 25; }
check() { name=$1; shift; if ( "$@" ) >"$out" 2>&1; then pass "$name"; else bad "$name"; fi; }
# Succeeds when the VM command fails and its output contains the text.
refused() { text=$1; shift; if v "$@" >"$out.r" 2>&1; then cat "$out.r"; return 1; fi; cat "$out.r"; grep -q "$text" "$out.r"; }

PASS1='correct horse battery'
PASS2='a brand new device passphrase'
BPASS='backup horse battery staple'
TOKEN='usb-restore-token-42'

# The helper, the ctl wrapper and a finder for the USB disk, on a VM that may be brand new.
prepare_vm() {
	vm push nodo-tools nodo-caddy nodo-localnet nodo-ddns >/dev/null 2>&1
	vm mirror dosfstools >/dev/null 2>&1
	v 'mkdir -p /tmp/e2e-bin
		printf "#!/bin/sh\nexec nodo-tui --ctl \"\$@\"\n" > /tmp/e2e-bin/ctl
		cat > /tmp/e2e-bin/usbdisk <<"EOF"
#!/bin/sh
# The first disk attached over USB.
for d in /sys/block/sd*; do readlink -f "$d" | grep -q /usb && { basename "$d"; exit 0; }; done; exit 1
EOF
		cat > /tmp/e2e-bin/running_for <<"EOF"
#!/bin/sh
# Succeeds once the service has stayed up for the given seconds.
for _ in $(seq 1 60); do
	up=$(sv status "$1" | sed -n "s/^run: [^ ]* (pid [0-9]*) \([0-9]*\)s.*/\1/p")
	[ -n "$up" ] && [ "$up" -ge "$2" ] && exit 0; sleep 1
done
sv status "$1"; exit 1
EOF
		chmod 755 /tmp/e2e-bin/ctl /tmp/e2e-bin/usbdisk /tmp/e2e-bin/running_for
		mkdir -p /etc/nodo && printf "directory = \"https://203.0.113.1/dir\"\n" > /etc/nodo/acme-test.toml
		[ -L /var/service/nodo-helper ] || ln -s /etc/sv/nodo-helper /var/service/
		for _ in $(seq 1 30); do [ -S /run/nodo/helper.sock ] && nodo-tui --ctl services >/dev/null 2>&1 && exit 0; sleep 1; done; exit 1'
}
P='PATH=/tmp/e2e-bin:$PATH; export PATH;'
wait_disk() { v "$P"'for _ in $(seq 1 20); do d=$(usbdisk) && [ -b /dev/${d}1 ] && exit 0; sleep 1; done; exit 1'; }

vm fresh >/dev/null
prepare_vm || { echo "FAIL the fresh VM did not come up with the helper"; exit 1; }

# The stick: 1 GB, one FAT32 partition, as drives come from the shop.
vm usb-create stick 1024
vm usb-plug stick >/dev/null
check "the virtual stick shows up as a removable USB disk" v "$P"'for _ in $(seq 1 20); do d=$(usbdisk) && break; sleep 1; done; [ "$(cat /sys/block/$d/removable)" = 1 ]'
v "$P"'d=$(usbdisk); printf "label: dos\n,,c\n" | sfdisk -q /dev/$d && sleep 1 && mkfs.vfat -n BACKUP /dev/${d}1 >/dev/null'
wait_disk
DEV=$(v "$P"'echo "$(usbdisk)1"')

check "only the stick is offered; the system disk never is" sh -c "
	\"$root/scripts/dev-vm.sh\" ssh 'nodo-tui --ctl usb-drives' | tee /dev/stderr | grep -q 'device: \"$DEV\"' &&
	! \"$root/scripts/dev-vm.sh\" ssh 'nodo-tui --ctl usb-drives' | grep -q 'device: \"vda'"
check "the system disk is refused by name" refused "not a removable drive" "printf '\n%s\n' '$BPASS' | nodo-tui --ctl usb-backup vda1"
check "a path instead of a drive name is refused" refused "NotFound" "printf '\n%s\n' '$BPASS' | nodo-tui --ctl usb-backup ../vda1"

# A USB disk carrying the data drive's label is never used, even though it is removable.
vm usb-create chain 64
vm usb-plug chain >/dev/null
sleep 3
CHAIN=$(v 'for d in /sys/block/sd*; do n=$(basename $d); [ "$n" != '"${DEV%1}"' ] && readlink -f $d | grep -q /usb && echo $n; done | head -n 1')
v "printf 'label: gpt\ntype=L, name=NODO_BLOCKCHAIN\n' | sfdisk -q /dev/$CHAIN && sleep 1 && mkfs.ext4 -q /dev/${CHAIN}1"
check "a USB disk labelled NODO_BLOCKCHAIN is refused as the data drive" \
	refused "data drive" "printf '\n%s\n' '$BPASS' | nodo-tui --ctl usb-backup ${CHAIN}1"
vm usb-unplug chain >/dev/null

# The device: a signed feed with an encrypted package that has a secret setting.
v 'cat > /tmp/usb-setup.sh' <<'SETUP'
set -eu
printf '%s\n' "$1" | nodo-tui --ctl init >/dev/null
FEED=/srv/usb-feed APPROOT=/tmp/usb-pkgs
rm -rf "$FEED" "$APPROOT"; mkdir -p "$FEED" "$APPROOT"
openssl genrsa -out /root/usb-feed-key.pem 2048 2>/dev/null
d=$APPROOT/e2e-notes
mkdir -p "$d/opt/nodo/apps/e2e-notes/bin"
chmod 755 "$d" "$d/opt" "$d/opt/nodo" "$d/opt/nodo/apps" "$d/opt/nodo/apps/e2e-notes" "$d/opt/nodo/apps/e2e-notes/bin"
cat > "$d/opt/nodo/apps/e2e-notes/manifest.toml" <<'EOF'
api_version = 1
id = "e2e-notes"
version = "1.0"
exec = ["bin/run"]
settings = "settings.lua"
stop_timeout_secs = 5

[storage]
encrypted = true
size_mib = 320
data_layout = 2

[[web]]
name = "notes"
upstream = "127.0.0.1:8090"
EOF
cat > "$d/opt/nodo/apps/e2e-notes/settings.lua" <<'EOF'
return { api_version = 1, settings = {
  { name = "token", type = "secret", label = "Token" },
} }
EOF
printf '#!/bin/sh\necho "e2e-notes running"\nwhile :; do sleep 1; done\n' > "$d/opt/nodo/apps/e2e-notes/bin/run"
chmod 644 "$d/opt/nodo/apps/e2e-notes/manifest.toml" "$d/opt/nodo/apps/e2e-notes/settings.lua"
chmod 755 "$d/opt/nodo/apps/e2e-notes/bin/run"
(cd "$APPROOT" && xbps-create -A x86_64 -n e2e-notes-1.0_1 -s "usb e2e app" "$d" >/dev/null)
cp "$APPROOT/e2e-notes-1.0_1.x86_64.xbps" "$FEED/"
xbps-rindex -a "$FEED/e2e-notes-1.0_1.x86_64.xbps" >/dev/null
xbps-rindex --signedby e2e --privkey /root/usb-feed-key.pem -s "$FEED" >/dev/null
xbps-rindex --privkey /root/usb-feed-key.pem -S "$FEED/e2e-notes-1.0_1.x86_64.xbps" >/dev/null
fp=$(nodo-tui --ctl probe-feed "$FEED" | sed -n 's/.*fingerprint: "\([^"]*\)".*/\1/p' | head -n 1)
nodo-tui --ctl add-feed "$FEED" "$fp" >/dev/null
printf '%s\n' "$1" | nodo-tui --ctl approve-install-encrypted usb-feed e2e-notes | grep -q "problem: None"
printf '%s\n' "$2" | nodo-tui --ctl set e2e-notes token secret >/dev/null
echo "kept in the volume" > /run/nodo/volumes/e2e-notes/data/notes.txt
chown _e2e-notes: /run/nodo/volumes/e2e-notes/data/notes.txt
# A public site, which starts Caddy.
nodo-tui --ctl host e2e-notes notes notes.example.com >/dev/null
nodo-tui --ctl expose e2e-notes notes 0 0 1 notes.example.com >/dev/null
grep -q notes.example.com /var/lib/nodo-web/caddy.json
# Caddy's storage as certificates leave it, owned by Caddy. Real certificates and keys, because
# the running Caddy reads and cleans its storage.
c=/var/lib/caddy/data/caddy
sv -w 30 stop nodo-caddy >/dev/null
mkdir -p "$c/certificates/acme-test/shop.example.com" "$c/pki/authorities/local"
openssl req -x509 -newkey ec -pkeyopt ec_paramgen_curve:P-256 -nodes -days 3650 -subj /CN=shop.example.com \
	-keyout "$c/certificates/acme-test/shop.example.com/shop.example.com.key" \
	-out "$c/certificates/acme-test/shop.example.com/shop.example.com.crt" 2>/dev/null
echo '{"sans":["shop.example.com"],"issuer_data":"private key"}' > "$c/certificates/acme-test/shop.example.com/shop.example.com.json"
rm -f "$c/pki/authorities/local/root.crt" "$c/pki/authorities/local/root.key"
openssl req -x509 -newkey ec -pkeyopt ec_paramgen_curve:P-256 -nodes -days 3650 -subj "/CN=Caddy Local Authority - E2E Root" \
	-keyout "$c/pki/authorities/local/root.key" -out "$c/pki/authorities/local/root.crt" 2>/dev/null
chown -R _caddy: /var/lib/caddy/data
sv start nodo-caddy >/dev/null
SETUP
check "the first device is set up with a feed, an encrypted package, a secret and a public site" v "sh /tmp/usb-setup.sh '$PASS1' '$TOKEN'"
# The files the setup gave Caddy, which a running Caddy leaves as they are. Caddy adds files of
# its own, such as its instance id and locks, which are not compared.
CADDY_FILES='/var/lib/caddy/data/caddy/certificates/acme-test /var/lib/caddy/data/caddy/pki/authorities/local/root.crt /var/lib/caddy/data/caddy/pki/authorities/local/root.key'
# What the restored device must have again.
v "cat /run/nodo/volumes/e2e-notes/data/notes.txt; cat /run/nodo/secrets/e2e-notes/env; find $CADDY_FILES -type f | sort | xargs sha256sum; nodo-tui --ctl feeds | sed -n 's/.*fingerprint: \"\\([^\"]*\\)\".*/\\1/p'" > "$root/build/usb-before.txt" 2>&1
check "the secret was rendered for the package" grep -q "$TOKEN" "$root/build/usb-before.txt"

check "a backup to the stick needs the device passphrase" refused "NeedsConfirmation" "printf '\n%s\n' '$BPASS' | nodo-tui --ctl usb-backup $DEV"
check "a backup is written to the stick and the stick is unmounted again" sh -c "
	\"$root/scripts/dev-vm.sh\" ssh \"printf '%s\n%s\n' '$PASS1' '$BPASS' | nodo-tui --ctl usb-backup $DEV\" | tee /dev/stderr | grep -q 'has_device_key: true' &&
	\"$root/scripts/dev-vm.sh\" ssh '! grep -q /run/nodo/usb /proc/mounts'"
check "the stick holds one complete backup in its own folder and no temporary file" v "
	mkdir -p /mnt/s && mount -o ro /dev/$DEV /mnt/s && ls -a /mnt/s/monokulo-nodo | tee /dev/stderr | grep -q '^nodo-[0-9]*.nodobak\$' &&
	! ls -a /mnt/s/monokulo-nodo | grep -q partial; r=\$?; umount /mnt/s; exit \$r"
check "the backup on the stick is encrypted" v "
	mount -o ro /dev/$DEV /mnt/s && ! grep -rq 'kept in the volume\|$TOKEN\|private key' /mnt/s; r=\$?; umount /mnt/s; exit \$r"
check "listing the stick shows it" sh -c "\"$root/scripts/dev-vm.sh\" ssh 'nodo-tui --ctl usb-backups $DEV' | tee /dev/stderr | grep -q 'nodo-[0-9]*.nodobak'"
NAME=$(v "nodo-tui --ctl usb-backups $DEV" | sed -n 's/.*name: "\(nodo-[0-9-]*\.nodobak\)".*/\1/p' | head -n 1)
check "inspecting it with a wrong passphrase is refused" refused "passphrase did not open" "printf 'wrong backup passphrase\n' | nodo-tui --ctl usb-inspect $DEV $NAME"
check "inspecting it lists the package and its storage" sh -c "
	\"$root/scripts/dev-vm.sh\" ssh \"printf '%s\n' '$BPASS' | nodo-tui --ctl usb-inspect $DEV $NAME\" | tee /dev/stderr | grep -q e2e-notes"

# A write-protected stick: still readable, never written.
vm usb-unplug stick >/dev/null; sleep 2
vm usb-plug stick on >/dev/null
wait_disk
check "a read-only stick is refused for a backup in plain words" refused "read-only. Check its write-protect switch" "printf '%s\n%s\n' '$PASS1' '$BPASS' | nodo-tui --ctl usb-backup $DEV"
check "a read-only stick can still be listed" sh -c "\"$root/scripts/dev-vm.sh\" ssh 'nodo-tui --ctl usb-backups $DEV' | grep -q '$NAME'"
vm usb-unplug stick >/dev/null; sleep 2

# A stick too small for the backup.
vm usb-create tiny 2
vm usb-plug tiny >/dev/null
wait_disk
TINY=$(v "$P"'echo "$(usbdisk)"')
v "printf 'label: dos\n,,c\n' | sfdisk -q /dev/$TINY && sleep 1 && mkfs.vfat /dev/${TINY}1 >/dev/null"
v "dd if=/dev/urandom of=/run/nodo/volumes/e2e-notes/data/big bs=1M count=200 status=none && chown _e2e-notes: /run/nodo/volumes/e2e-notes/data/big"
check "a full stick is refused in plain words, before writing" refused "The USB drive is full" "printf '%s\n%s\n' '$PASS1' '$BPASS' | nodo-tui --ctl usb-backup ${TINY}1"
vm usb-unplug tiny >/dev/null; sleep 2

# Pulled out while the backup is written: the cut-short file never gets a backup name.
vm usb-plug stick >/dev/null
wait_disk
v "(printf '%s\n%s\n' '$PASS1' '$BPASS' | nodo-tui --ctl usb-backup $DEV > /tmp/usb-yank.out 2>&1; echo done >> /tmp/usb-yank.out) >/dev/null 2>&1 &"
v 'for _ in $(seq 1 200); do s=$(stat -c %s /run/nodo/usb/monokulo-nodo/.nodo-*.partial 2>/dev/null | head -n 1); [ "${s:-0}" -gt 20000000 ] && exit 0; sleep 0.1; done; exit 1'
vm usb-unplug stick >/dev/null
check "a stick pulled out mid-write is reported in plain words" v '
	for _ in $(seq 1 120); do grep -q done /tmp/usb-yank.out && break; sleep 1; done; cat /tmp/usb-yank.out
	grep -q "unplugged or stopped answering" /tmp/usb-yank.out'
check "the helper unmounted it and still answers" v '! grep -q /run/nodo/usb /proc/mounts && nodo-tui --ctl services >/dev/null'
vm usb-plug stick >/dev/null
wait_disk
check "plugged back in, the stick lists only the complete backup" sh -c "
	\"$root/scripts/dev-vm.sh\" ssh 'nodo-tui --ctl usb-backups $DEV' | tee /dev/stderr | grep -c 'name: \"nodo-' | grep -qx 1"
v "rm -f /run/nodo/volumes/e2e-notes/data/big"
check "a second backup works, and the left-over partial file is cleaned up" sh -c "
	\"$root/scripts/dev-vm.sh\" ssh \"printf '%s\n%s\n' '$PASS1' '$BPASS' | nodo-tui --ctl usb-backup $DEV\" | grep -q BackupCreated &&
	\"$root/scripts/dev-vm.sh\" ssh 'mount -o ro /dev/$DEV /mnt/s && ! ls -a /mnt/s/monokulo-nodo | grep -q partial; r=\$?; umount /mnt/s; exit \$r'"
NAME=$(v "nodo-tui --ctl usb-backups $DEV" | sed -n 's/.*name: "\(nodo-[0-9-]*\.nodobak\)".*/\1/p' | head -n 1)

# The first device is gone. A clean image boots with the same stick.
vm usb-unplug stick >/dev/null
vm fresh >/dev/null
prepare_vm || { echo "FAIL the second VM did not come up"; exit 1; }
vm usb-plug stick >/dev/null
wait_disk
DEV=$(v "$P"'echo "$(usbdisk)1"')
check "the new device is not set up and has none of the old state" v '
	nodo-tui --ctl overview | grep -q "encryption: NotSetUp" && [ ! -e /var/lib/nodo/feeds.toml ] && ! xbps-query e2e-notes >/dev/null'
check "the new device lists the backup on the stick" sh -c "\"$root/scripts/dev-vm.sh\" ssh 'nodo-tui --ctl usb-backups $DEV' | grep -q '$NAME'"
check "a restore without the typed word is refused" refused "NeedsConfirmation" "printf '%s\n%s\n' '$BPASS' '$PASS2' | nodo-tui --ctl usb-restore $DEV $NAME yes"
check "a restore with a wrong backup passphrase is refused and sets nothing up" sh -c "
	! \"$root/scripts/dev-vm.sh\" ssh \"printf 'wrong backup passphrase\n%s\n' '$PASS2' | nodo-tui --ctl usb-restore $DEV $NAME restore\" &&
	\"$root/scripts/dev-vm.sh\" ssh 'nodo-tui --ctl overview | grep -q \"encryption: NotSetUp\"'"
check "restoring from the stick sets up the whole device" sh -c "
	\"$root/scripts/dev-vm.sh\" ssh \"printf '%s\n%s\n' '$BPASS' '$PASS2' | nodo-tui --ctl usb-restore $DEV $NAME restore\" | tee $root/build/usb-restore.txt;
	grep -q Restored $root/build/usb-restore.txt && grep -q 'from the verified copy in the backup' $root/build/usb-restore.txt"
check "the device is unlocked and the stick unmounted" v 'nodo-tui --ctl overview | grep -q "encryption: Unlocked" && ! grep -q /run/nodo/usb /proc/mounts'
check "the feed, its pinned key and the approval are back" sh -c "
	fp=\$(tail -n 1 $root/build/usb-before.txt); [ -n \"\$fp\" ] &&
	\"$root/scripts/dev-vm.sh\" ssh 'nodo-tui --ctl feeds' | grep -q \"fingerprint: \\\"\$fp\\\"\" &&
	\"$root/scripts/dev-vm.sh\" ssh 'grep -q e2e-notes /var/lib/nodo/feeds.toml && [ \"\$(stat -c \"%a %U\" /var/lib/nodo/feeds.toml)\" = \"600 root\" ]'"
check "the package is reinstalled at the backed-up version, offline, with its kept copy" v '
	[ "$(xbps-query -p pkgver e2e-notes)" = e2e-notes-1.0_1 ] && ls /var/lib/nodo/packages/e2e-notes/e2e-notes-1.0_1.x86_64.xbps && [ ! -e /srv/usb-feed ]'
check "its volume is recreated at the declared size, with the data and layout from the backup" v '
	[ "$(stat -c %s /var/lib/nodo/volumes/e2e-notes.luks)" = 335544320 ] && mountpoint -q /run/nodo/volumes/e2e-notes &&
	[ "$(cat /run/nodo/volumes/e2e-notes/data/notes.txt)" = "kept in the volume" ] &&
	[ "$(stat -c %U /run/nodo/volumes/e2e-notes/data/notes.txt)" = _e2e-notes ] && [ "$(cat /run/nodo/volumes/e2e-notes/.nodo-layout)" = 2 ]'
check "its service runs" v "$P"'running_for e2e-notes 3'
check "the sealed secret opens with the restored device key" sh -c "\"$root/scripts/dev-vm.sh\" ssh 'cat /run/nodo/secrets/e2e-notes/env' | grep -q '$TOKEN'"
check "Caddy's certificates and local CA are back, byte for byte, owned by Caddy" sh -c "
	\"$root/scripts/dev-vm.sh\" ssh 'find $CADDY_FILES -type f | sort | xargs sha256sum' > $root/build/usb-after-certs.txt &&
	grep /var/lib/caddy $root/build/usb-before.txt | diff - $root/build/usb-after-certs.txt &&
	\"$root/scripts/dev-vm.sh\" ssh '! find /var/lib/caddy/data -not -user _caddy | grep -q .'"
check "the restore applied the public site to Caddy at once, without another change" sh -c "
	grep -q 'site exposure applied to the proxy' $root/build/usb-restore.txt &&
	\"$root/scripts/dev-vm.sh\" ssh 'grep -q notes.example.com /var/lib/nodo-web/caddy.json && sv status nodo-caddy | grep -q \"^run:\"' &&
	\"$root/scripts/dev-vm.sh\" ssh 'nodo-tui --ctl sites' | grep -A8 'site: \"notes\"' | grep -q 'public: true'"
check "the public host name came back with it" v "grep -q notes.example.com /var/lib/nodo/public-hosts.toml"
check "after a restart of the helper, as after a reboot, the new passphrase unlocks the restored storage" v "
	sv -w 30 restart nodo-helper >/dev/null; sleep 1; for _ in \$(seq 1 30); do nodo-tui --ctl services >/dev/null 2>&1 && break; sleep 1; done
	sv -w 30 stop e2e-notes e2e-notes/log >/dev/null; umount /run/nodo/volumes/e2e-notes && cryptsetup close nodo-e2e-notes &&
	printf '%s\n' '$PASS2' | nodo-tui --ctl unlock && mountpoint -q /run/nodo/volumes/e2e-notes &&
	grep -q '$TOKEN' /run/nodo/secrets/e2e-notes/env && sv start e2e-notes >/dev/null"
check "the old device passphrase does not unlock it" refused "BadPassphrase" "printf '%s\n' '$PASS1' | nodo-tui --ctl unlock"
check "the helper log holds no passphrase or secret" v "! grep -E 'horse|brand new|$TOKEN' /var/log/nodo-helper/current"
vm usb-unplug stick >/dev/null

[ "$fail" = 0 ] && echo "ALL PASSED" || echo "SOME FAILED"
exit "$fail"
