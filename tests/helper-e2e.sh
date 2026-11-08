#!/bin/sh
# End-to-end test of nodo-helper inside the dev VM, as root, over its real socket, with real sv,
# nft, useradd, cryptsetup, mount and xfs_repair. Needs nodo-tools installed. It changes the VM
# (users, volumes, cuprated settings), so run it on a disposable VM:
#   scripts/dev-vm.sh push nodo-tools
#   scripts/dev-vm.sh ssh 'cat > /tmp/e2e.sh' < tests/helper-e2e.sh
#   scripts/dev-vm.sh ssh 'sh /tmp/e2e.sh </dev/null'
set -u
fail=0
out=/tmp/e2e.out
pass() { echo "PASS $1"; }
bad() { echo "FAIL $1"; fail=1; sed 's/^/    /' "$out" | head -n 25; }
# Runs a test body in a subshell. Its output is shown only on failure.
check() { name=$1; shift; if ( "$@" ) >"$out" 2>&1; then pass "$name"; else bad "$name"; fi; }
# A command rather than a function, so the sh -c bodies below can use it too.
mkdir -p /tmp/e2e-bin
printf '#!/bin/sh\nexec nodo-tui --ctl "$@"\n' > /tmp/e2e-bin/ctl
chmod 755 /tmp/e2e-bin/ctl
PATH=/tmp/e2e-bin:$PATH
export PATH
# Succeeds when the command fails and its output contains the text.
refused() { text=$1; shift; if "$@" >/tmp/e2e.r 2>&1; then cat /tmp/e2e.r; return 1; fi; cat /tmp/e2e.r; grep -q "$text" /tmp/e2e.r; }
wait_socket() { for _ in $(seq 1 30); do [ -S /run/nodo/helper.sock ] && ctl services >/dev/null 2>&1 && return 0; sleep 1; done; return 1; }
restart_helper() { sv -w 30 restart nodo-helper >/dev/null && sleep 1 && wait_socket; }
rpc_up() { for _ in $(seq 1 90); do grep -q ':46A1 00000000:0000 0A' /proc/net/tcp && return 0; sleep 2; done; return 1; }
running_for() { id=$1; secs=$2; for _ in $(seq 1 60); do up=$(sv status "$id" | sed -n 's/^run: [^ ]* (pid [0-9]*) \([0-9]*\)s.*/\1/p'); [ -n "$up" ] && [ "$up" -ge "$secs" ] && return 0; sleep 1; done; sv status "$id"; return 1; }
PASS1='correct horse battery'
PASS2='new horse battery staple'
TOKEN='s3cr3t-token-value'

# The service

[ -L /var/service/nodo-helper ] || ln -s /etc/sv/nodo-helper /var/service/
check "helper answers on its socket" wait_socket
check "socket is root:nodo 0660" sh -c '[ "$(stat -c "%a %U %G" /run/nodo/helper.sock)" = "660 root nodo" ]'
check "helper keeps its memory out of swap" sh -c 'p=$(pgrep -x nodo-helper); grep VmLck /proc/$p/status; ! grep -q "VmLck:[[:space:]]*0 kB" /proc/$p/status'

# Who may connect

check "a user outside the nodo group cannot connect" refused "Permission denied" chpst -u nobody nodo-tui --ctl services
id e2e-user >/dev/null 2>&1 || useradd -r -M e2e-user
usermod -aG nodo e2e-user
check "SO_PEERCRED denies a group member the helper was not started with" sh -c '
	chpst -u e2e-user:nodo nodo-tui --ctl services > /tmp/e2e.d 2>&1; cat /tmp/e2e.d; grep -q "Err(Denied)" /tmp/e2e.d'
check "the helper log names the denial without request data" grep -q "denied connection from uid" /var/log/nodo-helper/current
restart_helper
check "after a restart the new member is allowed" chpst -u e2e-user:nodo nodo-tui --ctl services
gpasswd -d e2e-user nodo >/dev/null
restart_helper

# Lua never runs as root

check "the settings worker refuses to run as root" refused "must not run as root" nodo-helper --settings-worker
check "an unknown package is not found" refused "NotFound" ctl form sshd

# cuprated settings: the real Cuprated.toml

check "cuprated form comes from its Lua schema" sh -c 'ctl form cuprated | grep -q outbound_peers'
cp /etc/cuprated/Cuprated.toml /tmp/Cuprated.before
check "a cuprated setting renders Cuprated.toml, restarts and passes its health check" sh -c '
	ctl set cuprated outbound_peers int 40 && grep -qx "outbound_connections = 40" /etc/cuprated/Cuprated.toml'
check "Cuprated.toml is readable only by root and _cuprated" sh -c '[ "$(stat -c "%a %U %G" /etc/cuprated/Cuprated.toml)" = "640 root _cuprated" ]'
check "cuprated runs on the new file and its RPC comes up" rpc_up
cp /etc/cuprated/Cuprated.toml /tmp/Cuprated.mid
check "an out of range value is refused before anything changes" refused "between 1 and 128" ctl set cuprated outbound_peers int 500
check "a typed reference lookalike is not a number, so it is refused" refused "whole number" ctl set cuprated outbound_peers text '$(cuprated.rpc_port)'
check "refusals left Cuprated.toml untouched" cmp /etc/cuprated/Cuprated.toml /tmp/Cuprated.mid
check "the settings store is root only" sh -c '[ "$(stat -c "%a %U" /var/lib/nodo/settings/cuprated.toml)" = "600 root" ]'

# An app installed by id: its own user, sandbox, firewall fragment and enabled service

mkdir -p /var/lib/nodo/manifests /opt/nodo/apps/e2e-app/bin /opt/nodo/apps/e2e-vault/bin
cat > /var/lib/nodo/manifests/e2e-app.toml <<'EOF'
api_version = 1
id = "e2e-app"
version = "1.0"
exec = ["bin/run"]
settings = "settings.lua"
stop_timeout_secs = 5

[[web]]
name = "shop"
upstream = "127.0.0.1:8080"
multi_address = true

[[web]]
name = "admin"
upstream = "127.0.0.1:8081"
EOF
cat > /opt/nodo/apps/e2e-app/bin/run <<'EOF'
#!/bin/sh
# Crashes when its settings say so, which is how the rollback test makes it unhealthy.
while :; do
	if grep -q 'mode = "crash"' /etc/apps/e2e-app/settings.toml 2>/dev/null; then echo crashing; exit 1; fi
	sleep 1
done
EOF
cat > /opt/nodo/apps/e2e-app/settings.lua <<'EOF'
return { api_version = 1, settings = {
  { name = "mode", type = "enum", label = "Mode", default = "normal", choices = { "normal", "crash" } },
  { name = "greeting", type = "string", label = "Greeting", default = "hello",
    validate = function(v) return #v <= 20, "must be 20 characters or fewer" end },
  { name = "token", type = "secret", label = "Token" },
  { name = "daemon_port", type = "int", label = "Daemon RPC port", default = "$(monero-daemon.rpc-port)" },
} }
EOF
cp /opt/nodo/apps/e2e-app/bin/run /opt/nodo/apps/e2e-vault/bin/run
chmod 755 /opt/nodo/apps/e2e-app/bin/run /opt/nodo/apps/e2e-vault/bin/run

check "install by id creates the user and enables the service" sh -c '
	nodo-tui --ctl install e2e-app && id _e2e-app && [ "$(readlink /var/service/e2e-app)" = /etc/sv/e2e-app ]'
check "the app starts and stays up" running_for e2e-app 3
# The first process of the user is bwrap itself, outside the sandbox, so look for the child.
check "the app runs as its own user in a sandbox" sh -c '
	for p in $(pgrep -u _e2e-app); do
		[ "$(readlink /proc/$p/ns/pid)" != "$(readlink /proc/1/ns/pid)" ] && [ ! -e /proc/$p/root/etc/shadow ] && exit 0
	done; ps -o pid,user,args -u _e2e-app; exit 1'
check "its firewall fragment is loaded" sh -c '[ -f /etc/nftables.d/e2e-app.nft ] && nft list ruleset | grep -qE "skuid (\"?_e2e-app\"?|$(id -u _e2e-app))"'
check "reinstalling an id the helper created is allowed" ctl install e2e-app
id _e2e-taken >/dev/null 2>&1 || useradd -r -M _e2e-taken
sed 's/e2e-app/e2e-taken/' /var/lib/nodo/manifests/e2e-app.toml > /var/lib/nodo/manifests/e2e-taken.toml
check "an id whose user already exists is refused" refused "already in use" ctl install e2e-taken
check "the refused id got no service" sh -c '[ ! -e /etc/sv/e2e-taken ] && [ ! -e /var/service/e2e-taken ]'
check "base services cannot be installed or touched" refused "NotFound" ctl install sshd

# App settings: validators in the worker, references, secrets

check "a valid app setting is written and the app restarts healthy" sh -c '
	ctl set e2e-app greeting text hi && grep -qx "greeting = \"hi\"" /etc/apps/e2e-app/settings.toml'
check "a reference to the node resolves to its port" grep -qx "daemon_port = 18081" /etc/apps/e2e-app/settings.toml
check "the app settings file is readable only by the app" sh -c '[ "$(stat -c "%a %U %G" /etc/apps/e2e-app/settings.toml)" = "640 root _e2e-app" ]'
check "a package validator rejects a value" refused "20 characters or fewer" ctl set e2e-app greeting text "this greeting is far too long"
check "a secret is accepted from stdin" sh -c 'printf "%s\n" "$0" | nodo-tui --ctl set e2e-app token secret' "$TOKEN"
check "the form reports the secret only as set" sh -c 'ctl form e2e-app | grep -q SecretSet && ! ctl form e2e-app | grep -q "$0"' "$TOKEN"

# Exposure

check "a public host can be set" ctl host e2e-app shop shop.example.com
check "going public without the typed host is refused" refused "NeedsConfirmation" ctl expose e2e-app shop 1 0 1
check "going public with the wrong host is refused" refused "NeedsConfirmation" ctl expose e2e-app shop 1 0 1 evil.example.com
check "going public with the typed host works" ctl expose e2e-app shop 1 0 1 shop.example.com
check "the site lists as public" sh -c 'ctl sites | grep -A6 "site: \"shop\"" | grep -q "public: true"'
check "a single-address site refuses two exposures" refused "single address" ctl expose e2e-app admin 1 1 0
check "a single-address site takes one" ctl expose e2e-app admin 0 1 0

# Encrypted storage with the single device passphrase

check "before setup, the device reports encryption as not set up" sh -c 'ctl overview | grep -q NotSetUp'
check "unlock before setup says so" refused "NotSetUp" sh -c 'printf "%s\n" "$0" | nodo-tui --ctl unlock' "$PASS1"
check "a short passphrase is refused" refused "at least 8" sh -c 'echo short | nodo-tui --ctl init'
check "first-boot setup creates the device key" sh -c 'printf "%s\n" "$0" | nodo-tui --ctl init && [ "$(stat -c "%a %U" /var/lib/nodo/keys/device.key)" = "600 root" ]' "$PASS1"
check "setup refuses to run twice" refused "already set up" sh -c 'printf "%s\n" "$0" | nodo-tui --ctl init' "$PASS2"
sed -e 's/e2e-app/e2e-vault/' -e '/^settings/d' -e '/^\[\[web\]\]/,$d' /var/lib/nodo/manifests/e2e-app.toml > /var/lib/nodo/manifests/e2e-vault.toml
printf '\n[storage]\nencrypted = true\n' >> /var/lib/nodo/manifests/e2e-vault.toml
check "an encrypted package needs the passphrase to install" refused "needs the device passphrase" ctl install e2e-vault
check "an encrypted install creates a real LUKS2 volume" sh -c '
	printf "%s\n" "$0" | nodo-tui --ctl install-encrypted e2e-vault &&
	cryptsetup isLuks --type luks2 /var/lib/nodo/volumes/e2e-vault.luks &&
	[ -e /dev/mapper/nodo-e2e-vault ] && mountpoint -q /run/nodo/volumes/e2e-vault &&
	[ "$(stat -c "%a %U" /run/nodo/volumes/e2e-vault)" = "700 _e2e-vault" ]' "$PASS1"
check "the vault service starts on its volume" running_for e2e-vault 3
check "closing the volume shows the service and device as locked" sh -c '
	sv -w 30 stop e2e-vault >/dev/null; umount /run/nodo/volumes/e2e-vault && cryptsetup close nodo-e2e-vault &&
	sv start e2e-vault >/dev/null && ctl services | grep -A3 "id: \"e2e-vault\"" | grep -q Locked &&
	ctl overview | grep -q "encryption: Locked"'
check "a wrong passphrase opens nothing and is delayed" sh -c '
	start=$(date +%s); printf "wrong horse battery\n" | nodo-tui --ctl unlock > /tmp/e2e.u 2>&1; s1=$?
	printf "wrong horse battery\n" | nodo-tui --ctl unlock >> /tmp/e2e.u 2>&1; s2=$?
	took=$(( $(date +%s) - start )); cat /tmp/e2e.u; echo "took ${took}s"
	[ $s1 -ne 0 ] && [ $s2 -ne 0 ] && grep -q BadPassphrase /tmp/e2e.u && [ "$took" -ge 6 ] && ! mountpoint -q /run/nodo/volumes/e2e-vault'
check "the passphrase unlocks the volume and the service starts" sh -c '
	printf "%s\n" "$0" | nodo-tui --ctl unlock && mountpoint -q /run/nodo/volumes/e2e-vault' "$PASS1"
check "the unlocked service runs" running_for e2e-vault 3
check "changing the passphrase keeps the volume" sh -c 'printf "%s\n%s\n" "$0" "$1" | nodo-tui --ctl change-passphrase' "$PASS1" "$PASS2"
check "after a change only the new passphrase unlocks" sh -c '
	sv -w 30 stop e2e-vault >/dev/null; umount /run/nodo/volumes/e2e-vault && cryptsetup close nodo-e2e-vault &&
	! printf "%s\n" "$0" | nodo-tui --ctl unlock && printf "%s\n" "$1" | nodo-tui --ctl unlock &&
	mountpoint -q /run/nodo/volumes/e2e-vault' "$PASS1" "$PASS2"
sv start e2e-vault >/dev/null

# SSH keys

check "the image's key is listed" sh -c '[ "$(ctl keys | grep -c fingerprint)" -eq 1 ]'
rm -f /tmp/e2e-key /tmp/e2e-key.pub
ssh-keygen -q -t ed25519 -N '' -C e2e-extra -f /tmp/e2e-key
check "a key line with options is refused" refused "options are not allowed" ctl add-key "command=\"sh\" $(cat /tmp/e2e-key.pub)"
check "a key can be added" ctl add-key "$(cat /tmp/e2e-key.pub)"
check "both keys are listed and the file stays private" sh -c '[ "$(ctl keys | grep -c fingerprint)" -eq 2 ] && [ "$(stat -c %a /root/.ssh/authorized_keys)" = 600 ]'
fp=$(ssh-keygen -lf /tmp/e2e-key.pub | cut -d' ' -f2)
check "a key can be removed by its fingerprint" ctl remove-key "$fp"
only=$(ctl keys | sed -n 's/.*fingerprint: "\(.*\)",/\1/p')
check "the last key cannot be removed" refused "only key" ctl remove-key "$only"
check "the test key still logs in" grep -q monokulo-test /root/.ssh/authorized_keys

# Logs

check "a service log can be read" sh -c 'ctl log cuprated 5 | grep -q "lines:"'
check "the helper log can be read" sh -c 'ctl log nodo-helper 5 | grep -q "nodo-helper: uid 0"'
check "a path is not a log source" refused "NotFound" ctl log ../../etc/shadow 5
check "the helper log holds no passphrase or secret" sh -c '! grep -E "horse|$0" /var/log/nodo-helper/current' "$TOKEN"

# The data drive, on a loop device

loopdev=$(losetup -j /var/tmp/e2e-drive.img | cut -d: -f1)
if [ -z "$loopdev" ]; then
	rm -f /var/tmp/e2e-drive.img
	truncate -s 320M /var/tmp/e2e-drive.img
	loopdev=$(losetup -f --show /var/tmp/e2e-drive.img)
fi
mkfs.xfs -q -f "$loopdev"
mkdir -p /media/e2e
grep -q ' /media/e2e ' /etc/fstab || echo "$loopdev /media/e2e xfs defaults,noauto 0 0" >> /etc/fstab
printf 'DRIVE=%s\nDRIVE_MOUNT=/media/e2e\n' "$loopdev" > /etc/sv/nodo-helper/conf
restart_helper
check "the drive is reported present and unmounted" sh -c 'ctl drive | grep -q "present: true" && ctl drive | grep -q "mounted: false" && ctl drive | grep -q xfs'
id_of() { ctl drive-check | sed -n 's/.*check_id: \([0-9]*\),/\1/p'; }
check "a clean drive checks clean and is not repaired" sh -c '
	ctl drive-check | grep -q "clean: true" && id=$(ctl drive-check | sed -n "s/.*check_id: \([0-9]*\),/\1/p") &&
	! ctl drive-repair "$id"'
check "a repair without a check is refused" refused "NeedsConfirmation" ctl drive-repair 12345
# Zeroing the root inode's mode is damage xfs_repair finds and fixes.
xfs_db -x -c 'sb 0' -c 'p rootino' "$loopdev" > /tmp/e2e.root 2>&1
root=$(sed -n 's/rootino = //p' /tmp/e2e.root)
xfs_db -x -c "inode $root" -c 'write core.mode 0' "$loopdev" >/dev/null 2>&1
check "a damaged drive checks as damaged" sh -c 'ctl drive-check | tee /tmp/e2e.c; grep -q "clean: false" /tmp/e2e.c'
check "a repair quoting a different check is refused" refused "NeedsConfirmation" ctl drive-repair 1
check "a repair quoting the failed check fixes the drive" sh -c '
	id=$(ctl drive-check | sed -n "s/.*check_id: \([0-9]*\),/\1/p"); ctl drive-repair "$id" | grep -q "clean: true" &&
	ctl drive-check | grep -q "clean: true"'
check "the drive mounts through fstab" sh -c 'ctl drive-mount && mountpoint -q /media/e2e && ctl drive | grep -q "mounted: true"'
check "a mounted drive is never checked" refused "mounted" ctl drive-check
umount /media/e2e

# Settings rollback, last because it waits for the crash loop to be noticed

check "a setting that makes the app crash is rolled back" sh -c '
	ctl set e2e-app mode text crash | tee /tmp/e2e.rb; grep -q RolledBack /tmp/e2e.rb && grep -q "recovered: true" /tmp/e2e.rb &&
	! grep -q crash /etc/apps/e2e-app/settings.toml'
check "the app runs again on its old settings" running_for e2e-app 3

echo "helper log tail:"
tail -n 5 /var/log/nodo-helper/current
exit "$fail"
