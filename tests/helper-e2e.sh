#!/bin/sh
# End-to-end test of nodo-helper inside the dev VM, as root, over its real socket, with real sv,
# nft, useradd, cryptsetup, mount and xfs_repair. Needs nodo-tools installed. It changes the VM
# (users, volumes, cuprated settings), so run it on a disposable VM:
#   scripts/dev-vm.sh push nodo-tools
#   scripts/build-packages.sh x86_64 monokulo     (optional: Monokulo installs from the test feed)
#   scripts/dev-vm.sh ssh 'cat > /tmp/monokulo.xbps' < out/feed/x86_64/monokulo-<version>.x86_64.xbps
#   echo monokulo-<version>.x86_64.xbps | scripts/dev-vm.sh ssh 'cat > /tmp/monokulo.name'
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

# Feeds and installs: a feed signed in the VM with a real RSA key, published with xbps-rindex

FEED=/srv/e2e-feed
APPROOT=/tmp/e2e-pkgs
rm -rf "$FEED" "$APPROOT" /root/e2e-feed-key.pem /root/e2e-feed-key2.pem
mkdir -p "$FEED" "$APPROOT"
openssl genrsa -out /root/e2e-feed-key.pem 2048 2>/dev/null
openssl genrsa -out /root/e2e-feed-key2.pem 2048 2>/dev/null
# Builds e2e-pay at a version with xbps-create. Mode "good" stays up, "crash" damages its data and
# exits, which is how an update fails its health check.
mkpkg() {
	ver=$1 mode=$2 extra=${3:-}
	d=$APPROOT/e2e-pay-$ver
	rm -rf "$d"
	mkdir -p "$d/opt/nodo/apps/e2e-pay/bin"
	chmod 755 "$d" "$d/opt" "$d/opt/nodo" "$d/opt/nodo/apps" "$d/opt/nodo/apps/e2e-pay" "$d/opt/nodo/apps/e2e-pay/bin"
	printf 'api_version = 1\nid = "e2e-pay"\nversion = "%s"\nexec = ["bin/run"]\nstop_timeout_secs = 5\n%s' "$ver" "$extra" \
		> "$d/opt/nodo/apps/e2e-pay/manifest.toml"
	if [ "$mode" = good ]; then
		printf '#!/bin/sh\necho "e2e-pay %s running"\nwhile :; do sleep 1; done\n' "$ver" > "$d/opt/nodo/apps/e2e-pay/bin/run"
	else
		printf '#!/bin/sh\necho broken > /var/lib/apps/e2e-pay/marker\necho "migration 14: table invoices already exists" >&2\nexit 1\n' \
			> "$d/opt/nodo/apps/e2e-pay/bin/run"
	fi
	chmod 644 "$d/opt/nodo/apps/e2e-pay/manifest.toml"
	chmod 755 "$d/opt/nodo/apps/e2e-pay/bin/run"
	(cd "$APPROOT" && xbps-create -A x86_64 -n "e2e-pay-${ver}_1" -s "e2e feed app" "$d" >/dev/null)
	publish "$APPROOT/e2e-pay-${ver}_1.x86_64.xbps"
}
publish() {
	key=${KEY:-/root/e2e-feed-key.pem}
	cp "$1" "$FEED/" && xbps-rindex -f -a "$FEED/$(basename "$1")" >/dev/null &&
		xbps-rindex --signedby e2e --privkey "$key" -s "$FEED" >/dev/null &&
		xbps-rindex --privkey "$key" -S "$FEED/$(basename "$1")" >/dev/null
}
field() { sed -n "s/.*$1: \"\{0,1\}\([^\",]*\)\"\{0,1\},\{0,1\}$/\1/p" | head -n 1; }
mkpkg 1.0 good
check "the signed test feed is published" sh -c "ls $FEED/e2e-pay-1.0_1.x86_64.xbps.sig2 && xbps-query -vL --repository=$FEED 2>&1 | tee /dev/stderr | grep -q 'Signed-by: e2e'"
ctl probe-feed "$FEED" > /tmp/e2e.probe
fp=$(field fingerprint < /tmp/e2e.probe)
check "probing a feed reports its key and changes nothing" sh -c '
	cat /tmp/e2e.probe; [ -n "$0" ] && grep -q "name: \"e2e-feed\"" /tmp/e2e.probe && ! ctl feeds | grep -q e2e-feed' "$fp"
check "a feed is refused when its key is not the one confirmed" refused "NeedsConfirmation" ctl add-feed "$FEED" "00112233445566778899aabbccddeeff"
check "a feed is added with the confirmed key" sh -c 'ctl add-feed "$1" "$0" && ctl feeds | grep -q "fingerprint: \"$0\""' "$fp" "$FEED"
check "the feed's packages are in the catalog" sh -c 'ctl catalog | grep -A3 "name: \"e2e-pay\"" | grep -q "version: \"1.0_1\""'
check "a review shows the permissions and installs nothing" sh -c '
	ctl review e2e-feed e2e-pay | tee /tmp/e2e.rev | grep -q "keeps its data unencrypted" && ! xbps-query e2e-pay >/dev/null'
check "an install without approval is refused" refused "NeedsConfirmation" ctl install-pkg e2e-feed e2e-pay e2e-pay-1.0_1
check "an approval for other permissions is refused" refused "NeedsConfirmation" ctl install-pkg e2e-feed e2e-pay e2e-pay-1.0_1 "connects to the internet"
check "the refused installs left nothing behind" sh -c '
	! xbps-query e2e-pay && [ ! -e /opt/nodo/apps/e2e-pay ] && [ ! -e /var/lib/nodo/manifests/e2e-pay.toml ] && ! id _e2e-pay'
check "an approved install goes through every step" sh -c '
	ctl approve-install e2e-feed e2e-pay | tee /tmp/e2e.inst; grep -q "problem: None" /tmp/e2e.inst'
check "xbps has the verified version in the app directory" sh -c '
	[ "$(xbps-query -p pkgver e2e-pay)" = e2e-pay-1.0_1 ] && [ -x /opt/nodo/apps/e2e-pay/bin/run ] &&
	[ "$(stat -c "%a %U" /var/lib/nodo/manifests/e2e-pay.toml)" = "644 root" ]'
check "the system xbps configuration was not touched" sh -c '! grep -rq e2e-feed /etc/xbps.d /usr/share/xbps.d'
check "the installed app runs as its own user" running_for e2e-pay 3
check "a copy of the verified package is kept for rollback" sh -c 'ls /var/lib/nodo/packages/e2e-pay/e2e-pay-1.0_1.x86_64.xbps'
check "the pins and approval are stored root only" sh -c '
	[ "$(stat -c "%a %U" /var/lib/nodo/feeds.toml)" = "600 root" ] && grep -q "keeps its data unencrypted\|unencrypted-storage" /var/lib/nodo/feeds.toml'

# A package breaking the install rules is rejected, with no override.
bad=$APPROOT/e2e-bad
mkdir -p "$bad/opt/nodo/apps/e2e-bad" "$bad/etc/cron.d"
chmod 755 "$bad" "$bad/opt" "$bad/opt/nodo" "$bad/opt/nodo/apps" "$bad/opt/nodo/apps/e2e-bad"
printf 'api_version = 1\nid = "e2e-bad"\nversion = "1.0"\nexec = ["bin/run"]\n' > "$bad/opt/nodo/apps/e2e-bad/manifest.toml"
echo '* * * * * root sh' > "$bad/etc/cron.d/e2e-bad"
printf '#!/bin/sh\nexit 0\n' > "$bad/INSTALL"
chmod 755 "$bad/INSTALL"
(cd "$APPROOT" && xbps-create -A x86_64 -n e2e-bad-1.0_1 -s "breaks the rules" "$bad" >/dev/null)
publish "$APPROOT/e2e-bad-1.0_1.x86_64.xbps"
ctl refresh-feeds >/dev/null
check "a package with a root script and files outside its folder is rejected" sh -c '
	ctl review e2e-feed e2e-bad | tee /tmp/e2e.bad; grep -q InstallRejected /tmp/e2e.bad && grep -q "install scripts" /tmp/e2e.bad &&
	grep -q "outside the package" /tmp/e2e.bad'
check "a rejected package cannot be installed by quoting its review" sh -c '! ctl approve-install e2e-feed e2e-bad | grep -q InstallReport'
check "the rejected package left nothing" sh -c '! xbps-query e2e-bad && [ ! -e /etc/cron.d/e2e-bad ]'

# Updates: more permissions wait for the user, a failed health check rolls back and is held

echo original > /var/lib/apps/e2e-pay/marker
mkpkg 1.1 good '[network]
outbound = "internet"
'
check "auto-update is off until a package is chosen" sh -c 'ctl auto-get | grep -q "enabled: \[\]"'
check "auto-update can be turned on for one package with a window" ctl auto-set daily 00:00 23:59 e2e-pay
check "a check finds the update and the permission it adds" sh -c '
	ctl check-updates | tee /tmp/e2e.up; grep -q "connects to the internet" /tmp/e2e.up && grep -q "waits for you: asks for new permissions" /tmp/e2e.up'
check "auto-update holds an update that asks for more" sh -c '
	ctl auto-run | tee /tmp/e2e.ar; grep -q UpdateResults /tmp/e2e.ar && ! grep -q "package:" /tmp/e2e.ar && [ "$(xbps-query -p pkgver e2e-pay)" = e2e-pay-1.0_1 ]'
check "selecting it without approval is refused" sh -c '
	ctl apply-update e2e-pay 1.1_1 | tee /tmp/e2e.ap; grep -q Refused /tmp/e2e.ap && [ "$(xbps-query -p pkgver e2e-pay)" = e2e-pay-1.0_1 ]'
mkpkg 1.2 crash
check "an update that fails its health check is found" sh -c 'ctl check-updates | grep -q "to: \"1.2_1\""'
check "auto-update rolls it back" sh -c '
	ctl auto-run | tee /tmp/e2e.rb2; grep -q RolledBack /tmp/e2e.rb2 && grep -q "recovered: true" /tmp/e2e.rb2'
check "the previous version is installed again" sh -c '[ "$(xbps-query -p pkgver e2e-pay)" = e2e-pay-1.0_1 ] && grep -q "\"1.0\"" /var/lib/nodo/manifests/e2e-pay.toml'
check "its data came back" sh -c '[ "$(cat /var/lib/apps/e2e-pay/marker)" = original ]'
check "the previous version runs" running_for e2e-pay 3
check "the failed version is held and shown for Home" sh -c '
	ctl update-status | tee /tmp/e2e.us; grep -q "held: true" /tmp/e2e.us && grep -A6 "attention:" /tmp/e2e.us | grep -q RolledBack'
check "a held version is not tried again" sh -c '
	ctl auto-run | tee /tmp/e2e.ar2; grep -q UpdateResults /tmp/e2e.ar2 && ! grep -q "package:" /tmp/e2e.ar2 && [ "$(xbps-query -p pkgver e2e-pay)" = e2e-pay-1.0_1 ]'
mkpkg 1.3 good
check "a healthy update installs automatically" sh -c '
	ctl check-updates >/dev/null && ctl auto-run | tee /tmp/e2e.ok; grep -q Updated /tmp/e2e.ok &&
	[ "$(xbps-query -p pkgver e2e-pay)" = e2e-pay-1.3_1 ]'
check "the updated app runs" running_for e2e-pay 3
check "only the current version's copy is kept" sh -c '[ "$(ls /var/lib/nodo/packages/e2e-pay)" = e2e-pay-1.3_1.x86_64.xbps ]'
check "the auto-update timer service asks the helper" sh -c '
	[ -L /var/service/nodo-autoupdate ] || ln -s /etc/sv/nodo-autoupdate /var/service/
	for _ in $(seq 1 20); do grep -q UpdateResults /var/log/nodo-autoupdate/current 2>/dev/null && exit 0; sleep 1; done; exit 1'
rm -f /var/service/nodo-autoupdate

# A changed feed key blocks the feed until the user types its name
KEY=/root/e2e-feed-key2.pem publish "$APPROOT/e2e-pay-1.3_1.x86_64.xbps"
check "a re-signed feed shows as key changed" sh -c 'ctl refresh-feeds | grep -q "key_changed: true"'
check "nothing installs from it" sh -c 'ctl review e2e-feed e2e-pay | tee /tmp/e2e.kc; grep -q "key_changed: true" /tmp/e2e.kc'
ctl feed-key e2e-feed > /tmp/e2e.key
old=$(field pinned < /tmp/e2e.key)
new=$(field offered < /tmp/e2e.key)
check "trusting the new key without the typed name is refused" refused "NeedsConfirmation" ctl trust-key e2e-feed "$old" "$new" e2e
check "trusting the new key with the typed name works" sh -c 'ctl trust-key e2e-feed "$0" "$1" e2e-feed | grep -q "key_changed: false"' "$old" "$new"

# Monokulo itself, from the same feed: encrypted storage, needs the node, outbound internet
if [ -f /tmp/monokulo.xbps ]; then
	cp /tmp/monokulo.xbps "$APPROOT/$(cat /tmp/monokulo.name)"
	KEY=/root/e2e-feed-key2.pem publish "$APPROOT/$(cat /tmp/monokulo.name)"
	ctl refresh-feeds >/dev/null
	check "Monokulo's review lists what it asks for" sh -c '
		ctl review e2e-feed monokulo | tee /tmp/e2e.mk; grep -q "uses monero-daemon" /tmp/e2e.mk &&
		grep -q "connects to the internet" /tmp/e2e.mk && grep -q "needs_passphrase: true" /tmp/e2e.mk'
	check "Monokulo needs the device passphrase for its storage" refused "NeedsConfirmation" ctl approve-install e2e-feed monokulo
	check "Monokulo installs with approval into encrypted storage" sh -c '
		printf "%s\n" "$0" | nodo-tui --ctl approve-install-encrypted e2e-feed monokulo > /tmp/e2e.mki
		sed -n "/InstallReport/,\$p" /tmp/e2e.mki | grep -E "name:|state:|problem|\"" | tee /dev/stderr |
			grep -A2 "\"Configure settings\"" | grep -q "state: Done" && xbps-query monokulo >/dev/null &&
		cryptsetup isLuks /var/lib/nodo/volumes/monokulo.luks && mountpoint -q /run/nodo/volumes/monokulo &&
		[ -L /var/service/monokulo ] && grep -q "^encryption_key" /var/lib/nodo/settings/monokulo.toml' "$PASS2"
	# Monokulo reads its key from the environment, which the service does not pass yet, so its
	# health check is expected to fail here. The install must say so rather than claim success.
	check "Monokulo's health check result is reported, not hidden" sh -c '
		grep -q "problem: None" /tmp/e2e.mki || grep -q "did not pass its health check" /tmp/e2e.mki'
	sv down monokulo
fi

# Backups and restore with the device key

mkdir -p /var/lib/nodo/secrets && echo keep-me > /var/lib/nodo/secrets/e2e
echo vault-data > /run/nodo/volumes/e2e-vault/e2e-file
chown _e2e-vault: /run/nodo/volumes/e2e-vault/e2e-file
ctl set e2e-app greeting text before >/dev/null
BACKUP_PASS='backup horse battery staple'
check "a backup without the device passphrase is refused" refused "NeedsConfirmation" sh -c 'printf "\n%s\n" "$0" | nodo-tui --ctl backup' "$BACKUP_PASS"
check "a backup with a wrong device passphrase is refused" refused "BadPassphrase" sh -c 'printf "%s\n%s\n" wrong-passphrase "$0" | nodo-tui --ctl backup' "$BACKUP_PASS"
check "a backup holds the device key and storage" sh -c '
	printf "%s\n%s\n" "$0" "$1" | nodo-tui --ctl backup | tee /tmp/e2e.bk; grep -q "has_device_key: true" /tmp/e2e.bk' "$PASS2" "$BACKUP_PASS"
bk=$(field name < /tmp/e2e.bk)
check "the backup file is root only and encrypted" sh -c '
	f=/var/lib/nodo/backups/$0; [ "$(stat -c "%a %U" "$f")" = "600 root" ] && head -c 8 "$f" | grep -q NODOBAK1 &&
	! grep -q "keep-me\|vault-data\|before" "$f"' "$bk"
check "inspecting with a wrong passphrase is refused" refused "passphrase did not open" sh -c 'printf "wrong backup passphrase\n" | nodo-tui --ctl inspect-backup "$0"' "$bk"
check "inspecting lists what a restore replaces" sh -c '
	printf "%s\n" "$1" | nodo-tui --ctl inspect-backup "$0" | tee /tmp/e2e.bi; grep -q e2e-vault /tmp/e2e.bi && grep -q "e2e-app" /tmp/e2e.bi' "$bk" "$BACKUP_PASS"
cp "/var/lib/nodo/backups/$bk" /var/lib/nodo/backups/nodo-tampered.nodobak
size=$(stat -c %s /var/lib/nodo/backups/nodo-tampered.nodobak)
printf 'X' | dd of=/var/lib/nodo/backups/nodo-tampered.nodobak bs=1 seek=$((size - 100)) conv=notrunc 2>/dev/null
ctl set e2e-app greeting text after >/dev/null
rm -f /var/lib/nodo/secrets/e2e /run/nodo/volumes/e2e-vault/e2e-file
check "a tampered backup is refused and changes nothing" refused "altered\\|damaged" sh -c '
	printf "%s\n%s\n" "$0" "new device pass" | nodo-tui --ctl restore nodo-tampered.nodobak restore' "$BACKUP_PASS"
check "a wrong passphrase restores nothing" refused "passphrase did not open" sh -c '
	printf "wrong backup passphrase\n%s\n" "new device pass" | nodo-tui --ctl restore "$0" restore' "$bk"
check "a restore without the typed word is refused" refused "NeedsConfirmation" sh -c '
	printf "%s\n%s\n" "$1" "new device pass" | nodo-tui --ctl restore "$0" yes' "$bk" "$BACKUP_PASS"
check "the refused restores changed nothing" sh -c '
	[ ! -e /var/lib/nodo/secrets/e2e ] && grep -qx "greeting = \"after\"" /etc/apps/e2e-app/settings.toml && ! ls -d /var/lib/nodo/.restore-* 2>/dev/null'
check "a restore stops the services it replaces, restores and starts them" sh -c '
	printf "%s\n%s\n" "$1" "new device pass" | nodo-tui --ctl restore "$0" restore | tee /tmp/e2e.rs
	grep -A4 "stopped:" /tmp/e2e.rs | grep -q e2e-vault && grep -A4 "restarted:" /tmp/e2e.rs | grep -q e2e-vault' "$bk" "$BACKUP_PASS"
check "settings, secrets and storage are back" sh -c '
	grep -qx "greeting = \"before\"" /etc/apps/e2e-app/settings.toml && [ "$(cat /var/lib/nodo/secrets/e2e)" = keep-me ] &&
	[ "$(cat /run/nodo/volumes/e2e-vault/e2e-file)" = vault-data ] && [ "$(stat -c %U /run/nodo/volumes/e2e-vault/e2e-file)" = _e2e-vault ]'
check "the device key used by this device's volumes was kept" sh -c '
	grep -q "uses its own key" /tmp/e2e.rs && printf "%s\n" "$0" | nodo-tui --ctl unlock' "$PASS2"
check "the restored services run" running_for e2e-vault 3
check "no staging copy is left behind" sh -c '! ls -d /var/lib/nodo/.restore-* 2>/dev/null'

# The real TUI against the real helper: feeds, packages, updates, Home's attention and backups

rm -f /tmp/e2e-tui.out
(
	sleep 4; printf '\t'; sleep 3; printf '\t'; sleep 2; printf '\t'; sleep 3
	printf '\t'; sleep 3; printf '\t'; sleep 2; printf '\t'; sleep 3
	printf 'b'; sleep 3; printf '\033'; sleep 2; printf 'q'; sleep 1
) | script -qfec 'stty rows 32 cols 100; TERM=xterm-256color nodo-tui' /tmp/e2e-tui.out >/dev/null 2>&1
sed 's/\x1b\[[0-9;?]*[A-Za-z]//g; s/\x1b[()][A-Z0-9]//g' /tmp/e2e-tui.out > /tmp/e2e-tui.txt
for want in "Your feeds" "e2e-feed" "e2e-pay" "e2e-bad" "Backups on this device" "$bk" "is back and the update is on hold"; do
	check "the TUI showed: $want" grep -q "$want" /tmp/e2e-tui.txt
done
check "the TUI no longer says feeds or updates are not available" sh -c '! grep -q "Not available yet: feeds\|Not available yet: automatic" /tmp/e2e-tui.txt'
check "the TUI never showed a passphrase" sh -c '! grep -q "horse" /tmp/e2e-tui.txt'

# Health: the daemon's configuration from manifests, and a low disk on a small loop filesystem

hloop=$(losetup -j /var/tmp/e2e-health.img | cut -d: -f1)
if [ -z "$hloop" ]; then
	rm -f /var/tmp/e2e-health.img
	# Sparse: a 6 GB filesystem is below the data drive's 20 GB warning but above its stop level.
	truncate -s 6G /var/tmp/e2e-health.img
	hloop=$(losetup -f --show /var/tmp/e2e-health.img)
fi
mkfs.xfs -q -f "$hloop"
mkdir -p /media/e2e-health
mount "$hloop" /media/e2e-health
printf 'DRIVE=%s\nDRIVE_MOUNT=/media/e2e-health\n' "$hloop" > /etc/sv/nodo-helper/conf
restart_helper
check "the helper writes the health configuration from installed manifests" sh -c '
	cat /etc/nodo/health.toml; grep -q "e2e-pay" /etc/nodo/health.toml && grep -q "/media/e2e-health" /etc/nodo/health.toml'
[ -L /var/service/nodo-healthd ] || ln -s /etc/sv/nodo-healthd /var/service/
check "nodo-healthd runs under runit and reports" sh -c '
	for _ in $(seq 1 30); do [ -s /run/nodo/health.json ] && ctl health | grep -q "reporting: true" && exit 0; sleep 1; done; ctl health; exit 1'
check "Home sees the low data disk" sh -c '
	ctl health | tee /tmp/e2e.h; grep -A6 "path: \"/media/e2e-health\"" /tmp/e2e.h | grep -q "level: \"Low\"" &&
	grep -q "data disk is low on space" /tmp/e2e.h'
check "a low data disk stops nothing" sh -c 'sv status cuprated | grep -q "^run:"'
echo "trial boots of the new system ran out, so the previous one was started" > /run/nodo/boot-fallback
check "the boot fallback notice reaches Home" sh -c 'ctl health | grep -q "boot_fallback: Some"'
rm -f /run/nodo/boot-fallback /var/service/nodo-healthd
sleep 2
umount /media/e2e-health
rm -f /etc/sv/nodo-helper/conf
restart_helper

# Settings rollback, last because it waits for the crash loop to be noticed

check "a setting that makes the app crash is rolled back" sh -c '
	ctl set e2e-app mode text crash | tee /tmp/e2e.rb; grep -q RolledBack /tmp/e2e.rb && grep -q "recovered: true" /tmp/e2e.rb &&
	! grep -q crash /etc/apps/e2e-app/settings.toml'
check "the app runs again on its old settings" running_for e2e-app 3

echo "helper log tail:"
tail -n 5 /var/log/nodo-helper/current
exit "$fail"
