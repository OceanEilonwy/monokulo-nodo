#!/bin/sh
# End-to-end test of nodo-helper inside the dev VM, as root, over its real socket, with real sv,
# nft, useradd, cryptsetup, mount and xfs_repair. Needs nodo-tools installed. It changes the VM
# (users, volumes, cuprated settings), so run it on a disposable VM. The web part also needs the web
# packages, curl, and tor for onion addresses:
#   scripts/dev-vm.sh push nodo-tools nodo-caddy nodo-localnet nodo-ddns
#   scripts/build-packages.sh x86_64 monokulo     (optional: Monokulo installs from the test feed)
#   scripts/dev-vm.sh ssh 'cat > /tmp/monokulo.xbps' < out/feed/x86_64/monokulo-<version>.x86_64.xbps
#   echo monokulo-<version>.x86_64.xbps | scripts/dev-vm.sh ssh 'cat > /tmp/monokulo.name'
#   tests/acme-prepare.sh                          (optional: real ACME certificates from Pebble)
#   scripts/dev-vm.sh ssh 'cat > /tmp/acme-e2e.sh' < tests/acme-e2e.sh
#   scripts/dev-vm.sh mirror python3; scripts/dev-vm.sh ssh 'cat > /tmp/abstract-sockets.sh' < tests/abstract-sockets.sh
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
# e2e-app has a secret setting, so its rendered settings live in memory, readable only after an
# unlock. Inside its sandbox they are still at /etc/apps/e2e-app.
APPCONF=/run/nodo/secrets/e2e-app/config/settings.toml
APPENV=/run/nodo/secrets/e2e-app/env
# The sandboxed process of a service: the first process of its user is bwrap, outside it.
cat > /tmp/e2e-bin/sandboxed <<'EOF'
#!/bin/sh
for p in $(pgrep -u "_$1"); do
	[ "$(readlink /proc/$p/ns/pid)" != "$(readlink /proc/1/ns/pid)" ] && { echo "$p"; exit 0; }
done
exit 1
EOF
# Connects to a loopback port as a service's user, which is what its firewall rules match.
cat > /tmp/e2e-bin/connects_as <<'EOF'
#!/bin/sh
exec timeout 5 chpst -u "_$1:_$1" bash -c "exec 3<>/dev/tcp/127.0.0.1/$2" 2>/dev/null
EOF
# Locks one encrypted volume as a reboot would: stops the service and its logger, which keeps
# its log inside the volume open, then closes the volume.
cat > /tmp/e2e-bin/lock_volume <<'EOF'
#!/bin/sh
sv -w 30 stop "$1" "$1/log" >/dev/null
umount "/run/nodo/volumes/$1" && cryptsetup close "nodo-$1"
EOF
chmod 755 /tmp/e2e-bin/sandboxed /tmp/e2e-bin/connects_as /tmp/e2e-bin/lock_volume

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
	ctl set e2e-app greeting text hi && grep -qx "greeting = \"hi\"" "$0"' "$APPCONF"
check "a reference to the node resolves to its port" grep -qx "daemon_port = 18081" "$APPCONF"
check "the app settings file is readable only by the app" sh -c '[ "$(stat -c "%a %U %G" "$0")" = "640 root _e2e-app" ]' "$APPCONF"
check "a package validator rejects a value" refused "20 characters or fewer" ctl set e2e-app greeting text "this greeting is far too long"
check "before setup, a secret is refused, since it would be sealed under the passphrase" \
	refused "set it up first" sh -c 'printf "%s\n" "$0" | nodo-tui --ctl set e2e-app token secret' "$TOKEN"

# Settings as environment, and the resolved command line

check "the app's settings reach it as environment from a root-only file" sh -c '
	[ "$(stat -c "%a %U" "$0")" = "600 root" ] && grep -qx "NODO_GREETING='"'hi'"'" "$0" &&
	p=$(sandboxed e2e-app) && tr "\0" "\n" < /proc/$p/environ | grep -qx "NODO_GREETING=hi"' "$APPENV"
check "settings never appear in the app's command line" sh -c '
	p=$(sandboxed e2e-app) && ! tr "\0" "\n" < /proc/$p/cmdline | grep -q -e hi -e 18081 &&
	! grep -q NODO_ /etc/sv/e2e-app/run'

# Grants from references: a reference to an endpoint opens exactly that port, and only while it exists

check "the reference to the node's RPC port opens it in the app's firewall" sh -c '
	grep -qE "oifname \"lo\" tcp dport .*\b18081\b.* accept" /etc/nftables.d/e2e-app.nft &&
	nft list ruleset | grep -qE "skuid (\"?_e2e-app\"?|$(id -u _e2e-app)) oifname \"lo\" tcp dport .*\b18081\b"'
check "so the app's user can connect to the node" connects_as e2e-app 18081
check "a literal port replaces the reference and the app restarts" ctl set e2e-app daemon_port int 18081
check "a removed reference removes its firewall access" sh -c '
	! grep -q 18081 /etc/nftables.d/e2e-app.nft && ! nft list ruleset | grep -qE "skuid (\"?_e2e-app\"?|$(id -u _e2e-app)) .*18081" &&
	grep -qx "daemon_port = 18081" "$0"' "$APPCONF"
check "so the app's user can no longer connect to the node" sh -c '! connects_as e2e-app 18081'
check "putting the reference back restores the access" sh -c '
	ctl set e2e-app daemon_port ref monero-daemon.rpc-port && connects_as e2e-app 18081'

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
	[ -e /dev/mapper/nodo-e2e-vault ] && mountpoint -q /run/nodo/volumes/e2e-vault' "$PASS1"
check "the volume holds config, log and data, and its user cannot replace config" sh -c '
	v=/run/nodo/volumes/e2e-vault
	[ "$(stat -c "%a %U" $v)" = "711 root" ] && [ "$(stat -c "%a %U" $v/data)" = "750 _e2e-vault" ] &&
	[ "$(stat -c "%a %U" $v/log)" = "750 _e2e-vault" ] && [ "$(stat -c "%U" $v/config)" = root ]'
check "the vault service starts on its volume" running_for e2e-vault 3
check "its log is written inside the volume, never to the system disk" sh -c '
	for _ in $(seq 1 10); do [ -s /run/nodo/volumes/e2e-vault/log/current ] && break; sleep 1; done
	[ -e /run/nodo/volumes/e2e-vault/log/current ] && [ ! -e /var/log/apps/e2e-vault ]'
check "closing the volume shows the service and device as locked" sh -c '
	lock_volume e2e-vault && sv start e2e-vault/log e2e-vault >/dev/null &&
	ctl services | grep -A3 "id: \"e2e-vault\"" | grep -q Locked && ctl overview | grep -q "encryption: Locked"'
check "while locked, its logger waits too, and nothing is logged in clear" sh -c '
	sleep 6; [ ! -e /var/log/apps/e2e-vault ] && ! mountpoint -q /run/nodo/volumes/e2e-vault &&
	[ -z "$(ls -A /run/nodo/volumes/e2e-vault)" ]'
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
	lock_volume e2e-vault &&
	! printf "%s\n" "$0" | nodo-tui --ctl unlock && printf "%s\n" "$1" | nodo-tui --ctl unlock &&
	mountpoint -q /run/nodo/volumes/e2e-vault' "$PASS1" "$PASS2"
sv start e2e-vault/log e2e-vault >/dev/null

# Sealed secrets: stored only sealed under the device key, readable only after an unlock

check "a secret is accepted from stdin" sh -c 'printf "%s\n" "$0" | nodo-tui --ctl set e2e-app token secret' "$TOKEN"
check "the form reports the secret only as set" sh -c 'ctl form e2e-app | grep -q SecretSet && ! ctl form e2e-app | grep -q "$0"' "$TOKEN"
check "the settings store holds the secret only sealed" sh -c '
	f=/var/lib/nodo/settings/e2e-app.toml; [ "$(stat -c "%a %U" $f)" = "600 root" ] &&
	grep -q "sealed = \"v1\." $f && ! grep -q "$0" $f' "$TOKEN"
check "the secret exists in clear only in memory, where the app reads it" sh -c '
	grep -q "token = \"$0\"" "$1" && grep -qx "NODO_TOKEN='"'"'$0'"'"'" "$2" &&
	! grep -rqs "$0" /etc /var/lib /var/log /opt' "$TOKEN" "$APPCONF" "$APPENV"
check "the app gets it as environment, never as an argument" sh -c '
	p=$(sandboxed e2e-app) && tr "\0" "\n" < /proc/$p/environ | grep -qx "NODO_TOKEN=$0" &&
	! tr "\0" "\n" < /proc/$p/cmdline | grep -q "$0"' "$TOKEN"
# A reboot loses the key, which lives only in the helper's memory, and /run, which is memory.
# Restarting the helper and clearing /run/nodo/secrets is exactly that for everything involved.
restart_helper
rm -rf /run/nodo/secrets
sv restart e2e-app >/dev/null
check "after the key is lost, the device reports itself locked" sh -c 'ctl overview | grep -q "encryption: Locked"'
check "a sealed secret is unreadable while locked" sh -c '
	! grep -rqs "$0" /etc /var/lib /var/log /opt /run && grep -q "sealed = \"v1\." /var/lib/nodo/settings/e2e-app.toml' "$TOKEN"
check "while locked, nothing that needs the secret can change" \
	refused "sealed until the device is unlocked" ctl set e2e-app greeting text locked
check "the app waits for its settings instead of starting without them" sh -c '
	sleep 6; ! sandboxed e2e-app && grep -q "waiting: settings for e2e-app are sealed" /var/log/apps/e2e-app/current'
check "unlocking renders the secret again and the app starts on it" sh -c '
	printf "%s\n" "$0" | nodo-tui --ctl unlock && grep -qx "NODO_TOKEN='"'"'$1'"'"'" "$2"' "$PASS2" "$TOKEN" "$APPENV"
check "the unlocked app runs with the secret" sh -c '
	for _ in $(seq 1 20); do p=$(sandboxed e2e-app) && break; sleep 1; done
	tr "\0" "\n" < /proc/$p/environ | grep -qx "NODO_TOKEN=$0"' "$TOKEN"
# An older helper stored secrets in clear. The first unlock seals them.
sv -w 30 stop nodo-helper >/dev/null
printf '[values.token]\nliteral = "legacy-clear-token"\n' > /var/lib/nodo/settings/e2e-app.toml
sv start nodo-helper >/dev/null; wait_socket
check "a secret stored in clear before sealing existed is sealed at the first unlock" sh -c '
	printf "%s\n" "$0" | nodo-tui --ctl unlock && ! grep -q legacy-clear-token /var/lib/nodo/settings/e2e-app.toml &&
	grep -q "sealed = \"v1\." /var/lib/nodo/settings/e2e-app.toml && grep -q legacy-clear-token "$1"' "$PASS2" "$APPENV"
printf '%s\n' "$TOKEN" | nodo-tui --ctl set e2e-app token secret >/dev/null
ctl set e2e-app greeting text hi >/dev/null

# Dependents: a public setting change re-renders every package referencing it, as one change

mkdir -p /opt/nodo/apps/e2e-prov/bin /opt/nodo/apps/e2e-dep/bin
cat > /var/lib/nodo/manifests/e2e-prov.toml <<'EOF'
api_version = 1
id = "e2e-prov"
version = "1.0"
exec = ["bin/run"]
settings = "settings.lua"
stop_timeout_secs = 5

[[network.inbound]]
port = 7101
protocol = "tcp"
scope = "local"

[[network.inbound]]
port = 7102
protocol = "tcp"
scope = "local"

[[network.inbound]]
port = 7103
protocol = "tcp"
scope = "local"
EOF
cat > /opt/nodo/apps/e2e-prov/settings.lua <<'EOF'
return { api_version = 1, settings = {
  { name = "port", type = "int", label = "API port", default = 7101, min = 1024, max = 65535,
    public = "api-port", endpoint = "tcp" },
  { name = "api_key", type = "secret", label = "API key", generate = { hex = 16 } },
} }
EOF
sed 's/e2e-prov/e2e-dep/' /var/lib/nodo/manifests/e2e-prov.toml | sed '/^\[\[network/,$d' > /var/lib/nodo/manifests/e2e-dep.toml
cat > /opt/nodo/apps/e2e-dep/settings.lua <<'EOF'
return { api_version = 1, settings = {
  { name = "upstream", type = "int", label = "Provider port", default = "$(e2e-prov.api-port)" },
  { name = "prov_key", type = "secret", label = "Provider key" },
} }
EOF
printf '#!/bin/sh\nwhile :; do sleep 1; done\n' > /opt/nodo/apps/e2e-prov/bin/run
cat > /opt/nodo/apps/e2e-dep/bin/run <<'EOF'
#!/bin/sh
# Cannot work with its provider on port 7102, which is how the test makes a dependent fail.
[ "$NODO_UPSTREAM" = 7102 ] && { echo "cannot use port 7102"; exit 1; }
while :; do sleep 1; done
EOF
chmod 755 /opt/nodo/apps/e2e-prov/bin/run /opt/nodo/apps/e2e-dep/bin/run
check "a provider and a package referencing it install" sh -c '
	ctl install e2e-prov && ctl install e2e-dep && grep -qx "NODO_UPSTREAM='"'7101'"'" /run/nodo/secrets/e2e-dep/env'
check "both run" sh -c 'for _ in $(seq 1 30); do sandboxed e2e-prov >/dev/null && sandboxed e2e-dep >/dev/null && exit 0; sleep 1; done; exit 1'
check "the dependent's firewall opens the provider's port" grep -q "tcp dport 7101 accept" /etc/nftables.d/e2e-dep.nft
cp /var/lib/nodo/settings/e2e-prov.toml /tmp/e2e-prov.before
cp /run/nodo/secrets/e2e-dep/env /tmp/e2e-dep-env.before
cp /etc/nftables.d/e2e-dep.nft /tmp/e2e-dep-nft.before
check "a failing dependent rolls back the whole change, including the provider" sh -c '
	ctl set e2e-prov port int 7102 | tee /tmp/e2e.dep; grep -q RolledBack /tmp/e2e.dep &&
	grep -q "e2e-dep depends on this change" /tmp/e2e.dep && grep -q "recovered: true" /tmp/e2e.dep'
check "the provider is back on its old settings" sh -c '
	cmp /var/lib/nodo/settings/e2e-prov.toml /tmp/e2e-prov.before && grep -qx "NODO_PORT='"'7101'"'" /run/nodo/secrets/e2e-prov/env'
check "the dependent is back on its old settings and firewall" sh -c '
	cmp /run/nodo/secrets/e2e-dep/env /tmp/e2e-dep-env.before && cmp /etc/nftables.d/e2e-dep.nft /tmp/e2e-dep-nft.before &&
	nft list ruleset | grep -q "tcp dport 7101 accept"'
check "and both run again" sh -c 'for _ in $(seq 1 30); do sandboxed e2e-prov >/dev/null && sandboxed e2e-dep >/dev/null && exit 0; sleep 1; done; exit 1'
check "a working change re-renders and restarts the dependent too" sh -c '
	ctl set e2e-prov port int 7103 && grep -qx "NODO_UPSTREAM='"'7103'"'" /run/nodo/secrets/e2e-dep/env &&
	grep -q "tcp dport 7103 accept" /etc/nftables.d/e2e-dep.nft && ! grep -q 7101 /etc/nftables.d/e2e-dep.nft &&
	p=$(sandboxed e2e-dep) && tr "\0" "\n" < /proc/$p/environ | grep -qx NODO_UPSTREAM=7103'

# Stores: role bindings and approved secret grants

check "roles list the node and its provider" sh -c 'ctl roles | grep -A4 "role: \"monero-daemon\"" | grep -q cuprated'
check "a role binding is stored root-only" sh -c '
	ctl bind monero-daemon cuprated && [ "$(stat -c "%a %U" /var/lib/nodo/bindings.toml)" = "600 root" ] &&
	grep -q "monero-daemon = \"cuprated\"" /var/lib/nodo/bindings.toml'
check "a role can only be bound to a package providing it" refused "does not provide" ctl bind monero-daemon e2e-prov
check "reading another package's secret needs an approved grant" refused "needs approval" ctl set e2e-dep prov_key ref e2e-prov.api_key
check "an approved grant is stored and lets the reference resolve" sh -c '
	ctl grant e2e-dep e2e-prov api_key && grep -q "setting = \"api_key\"" /var/lib/nodo/bindings.toml &&
	ctl set e2e-dep prov_key ref e2e-prov.api_key &&
	key=$(sed -n "s/^NODO_API_KEY=.\(.*\).$/\1/p" /run/nodo/secrets/e2e-prov/env) && [ ${#key} -eq 32 ] &&
	grep -qx "NODO_PROV_KEY='"'"'$key'"'"'" /run/nodo/secrets/e2e-dep/env'
check "the grant survives a helper restart" sh -c 'sv -w 30 restart nodo-helper >/dev/null; sleep 2; ctl grants | grep -q api_key'
printf '%s\n' "$PASS2" | nodo-tui --ctl unlock >/dev/null
check "a grant still in use cannot be revoked" sh -c '
	! ctl revoke e2e-dep e2e-prov api_key >/dev/null 2>&1 && ctl grants | grep -q api_key'

# Node status, from cuprated's RPC through the helper, as Home shows it

check "the helper reports the node's height and peers" sh -c '
	ctl node | tee /tmp/e2e.node; grep -q "provider: \"cuprated\"" /tmp/e2e.node && grep -q "height: 1," /tmp/e2e.node &&
	grep -q "outgoing_peers: 0" /tmp/e2e.node'
# The real TUI on a pseudo-terminal. It skips unchanged cells with cursor moves, so the screen
# is compared with escapes and spaces removed.
check "Home shows the node's height and peers" sh -c '
	TERM=xterm script -qc "stty rows 40 cols 120; timeout 10 nodo-tui" /tmp/e2e.tui >/dev/null 2>&1 </dev/null
	sed "s/\x1b\[[0-9;?]*[a-zA-Z]//g" /tmp/e2e.tui | tr -d " " > /tmp/e2e.tui.txt
	grep -q "1(synced)" /tmp/e2e.tui.txt && grep -q "0out,0in" /tmp/e2e.tui.txt'

# Exposure, applied for real: Caddy, nftables, the local address, mDNS, Tor and dynamic DNS.
# The device's main address is 192.168.77.2 on a LAN namespace and 203.0.113.2 towards an internet
# namespace. Its local address is 192.168.77.3, static, because this LAN has no DHCP server.

onion_host=
web_down() {
	for s in nodo-caddy nodo-mdns nodo-localnet nodo-ddns tor; do
		[ -d "/etc/sv/$s" ] && { touch "/etc/sv/$s/down"; sv -w 15 down "$s" >/dev/null 2>&1; }
	done
	pkill -u nobody -x nodo-caddy 2>/dev/null
	for ns in e2elan e2einet; do ip netns pids "$ns" 2>/dev/null | xargs -r kill 2>/dev/null; ip netns del "$ns" 2>/dev/null; done
	ip link del e2elan0 2>/dev/null; ip link del e2einet0 2>/dev/null
	rm -f /etc/nftables.d/nodo-web.nft /etc/sv/nodo-localnet/conf
	nft -f /etc/nftables.conf
}
web_down
# Public sites ask Pebble's address, never Let's Encrypt. The VM is offline either way.
mkdir -p /etc/nodo
printf 'directory = "https://203.0.113.1/dir"\n' > /etc/nodo/acme-test.toml
ip netns add e2elan; ip netns add e2einet
ip link add e2elan0 type veth peer name eth0 netns e2elan
ip link add e2einet0 type veth peer name eth0 netns e2einet
ip addr add 192.168.77.2/24 dev e2elan0; ip addr add 203.0.113.2/24 dev e2einet0
ip link set e2elan0 up; ip link set e2einet0 up
ip netns exec e2elan ip addr add 192.168.77.1/24 dev eth0
ip netns exec e2einet ip addr add 203.0.113.1/24 dev eth0
for ns in e2elan e2einet; do ip netns exec "$ns" ip link set lo up; ip netns exec "$ns" ip link set eth0 up; done
ip netns exec e2einet ip route add 192.168.77.0/24 via 203.0.113.2
printf 'MODE=static\nPARENT=e2elan0\nADDRESS=192.168.77.3/24\n' > /etc/sv/nodo-localnet/conf
# The app itself only sleeps, so a stand-in answers on its upstream port with the path it was sent.
mkdir -p /tmp/e2e-up
printf '{"admin":{"disabled":true},"apps":{"http":{"servers":{"u":{"listen":["127.0.0.1:8080"],"automatic_https":{"disable":true},"routes":[{"handle":[{"handler":"static_response","body":"e2e shop {http.request.uri}"}]}]}}}}}' > /tmp/e2e-up/config.json
chown -R nobody /tmp/e2e-up
HOME=/tmp/e2e-up XDG_DATA_HOME=/tmp/e2e-up nohup chpst -u nobody nodo-caddy run --config /tmp/e2e-up/config.json >/tmp/e2e-up/log 2>&1 </dev/null &
# The request helpers go in a file, because each check runs in its own sh -c.
cat > /tmp/e2e-web-lib.sh <<'LIB'
cacert=/var/lib/caddy/data/caddy/pki/authorities/local/root.crt
# Prints the HTTP status, or ERR and curl's exit code when no response came back.
web_get() {
	ns=$1; shift
	if [ "$ns" = host ]; then set -- curl "$@"; else set -- ip netns exec "$ns" curl "$@"; fi
	: > /tmp/e2e.body
	code=$("$@" -s -o /tmp/e2e.body -w '%{http_code}' --cacert "$cacert" --connect-timeout 3 --max-time 6 2>/dev/null) || code="ERR$?"
	echo "$code"
}
is() { [ "$1" = "$2" ] || { echo "want $2, got $1"; cat /tmp/e2e.body; return 1; }; }
# SHA-256 of the root certificate Caddy's local CA uses, as browsers show it.
root_fp() { openssl x509 -noout -fingerprint -sha256 -in "$cacert" | cut -d= -f2; }
# Waits up to $1 seconds for a command to succeed.
until_ok() { n=$1; shift; for _ in $(seq 1 "$n"); do "$@" >/dev/null 2>&1 && return 0; sleep 1; done; "$@"; }
applied() { [ "$(cat /var/lib/nodo-web/local-address 2>/dev/null)" = "$1" ]; }
mdns_says() { [ "$(ip netns exec e2elan nodo-mdns --query "$1" --via 192.168.77.1 2>/dev/null)" = "$2" ]; }
LIB

check "a public host can be set" ctl host e2e-app shop shop.example.com
check "local exposure brings up the local address and Caddy" sh -c '
	ctl expose e2e-app shop 1 0 0 && grep -qx "address=192.168.77.3" /run/nodo-localnet/state &&
	sv status nodo-caddy | grep -q "^run:" && [ "$(stat -c "%a %U %G" /var/lib/nodo-web/caddy.json)" = "640 root _caddy" ]'
sleep 2
check "the local site answers at https://shop.local with no port" sh -c '
	. /tmp/e2e-web-lib.sh; is "$(web_get e2elan --resolve shop.local:443:192.168.77.3 https://shop.local/admin)" 200 &&
	grep -q "e2e shop /admin" /tmp/e2e.body'
check "mDNS answers shop.local with the local address" sh -c '
	[ "$(ip netns exec e2elan nodo-mdns --query shop.local --via 192.168.77.1)" = 192.168.77.3 ]'
check "the main address serves no local host, as a masquerading router would deliver it" sh -c '
	. /tmp/e2e-web-lib.sh; r=$(web_get e2elan -k --resolve shop.local:443:192.168.77.2 https://shop.local/admin);
	! grep -q "e2e shop" /tmp/e2e.body && [ "${r#ERR}" != "$r" ]'
check "the local address is closed to the internet" sh -c '
	. /tmp/e2e-web-lib.sh; is "$(web_get e2einet --resolve shop.local:443:192.168.77.3 https://shop.local/)" ERR28'

# The local CA trust page: plain HTTP on the local address's port 80, for the LAN only.
check "the trust page serves the exact root Caddy uses, in DER and PEM" sh -c '
	. /tmp/e2e-web-lib.sh; fp=$(root_fp); echo "root $fp"; [ -n "$fp" ] &&
	is "$(web_get e2elan -D /tmp/e2e.h --resolve nodo.local:80:192.168.77.3 http://nodo.local/nodo-local-ca.crt)" 200 &&
	grep -qi "^content-type: application/x-x509-ca-cert" /tmp/e2e.h &&
	[ "$(openssl x509 -inform DER -in /tmp/e2e.body -noout -fingerprint -sha256 | cut -d= -f2)" = "$fp" ] &&
	is "$(web_get e2elan --resolve nodo.local:80:192.168.77.3 http://nodo.local/nodo-local-ca.pem)" 200 &&
	[ "$(openssl x509 -in /tmp/e2e.body -noout -fingerprint -sha256 | cut -d= -f2)" = "$fp" ]'
check "the page and the helper show the same fingerprint, with install steps" sh -c '
	. /tmp/e2e-web-lib.sh; fp=$(root_fp)
	is "$(web_get e2elan --resolve nodo.local:80:192.168.77.3 http://nodo.local/)" 200 &&
	grep -q "$(echo "$fp" | cut -c1-47)" /tmp/e2e.body && grep -q "$(echo "$fp" | cut -c49-)" /tmp/e2e.body &&
	grep -q Android /tmp/e2e.body && grep -q iPhone /tmp/e2e.body && grep -q Windows /tmp/e2e.body &&
	grep -q Firefox /tmp/e2e.body && ctl local-status | tr -d " \n" | tee /tmp/e2e.ls &&
	grep -q "ca_fingerprint:Some(\"$fp\"" /tmp/e2e.ls && grep -q "trust_url:Some(\"http://nodo.local\"" /tmp/e2e.ls &&
	ctl certs | tr -d " \n" | grep -q "local_ca_fingerprint:Some(\"$fp\""'
check "mDNS answers nodo.local, and the bare local address serves the page" sh -c '
	. /tmp/e2e-web-lib.sh; mdns_says nodo.local 192.168.77.3 &&
	is "$(web_get e2elan http://192.168.77.3/)" 200 && grep -q "Trust this device" /tmp/e2e.body &&
	is "$(web_get e2elan --resolve nodo.local:80:192.168.77.3 http://nodo.local/other)" 404 &&
	is "$(web_get e2elan -H "Host: shop.local" http://192.168.77.3/)" ERR52'
check "the trust page is unreachable from the internet side and its port is closed" sh -c '
	. /tmp/e2e-web-lib.sh
	is "$(web_get e2einet --resolve nodo.local:80:192.168.77.3 http://nodo.local/)" ERR28 &&
	is "$(web_get e2einet --resolve nodo.local:10081:192.168.77.3 http://nodo.local:10081/)" ERR28 &&
	is "$(web_get e2elan --resolve nodo.local:10081:192.168.77.3 http://nodo.local:10081/)" ERR28 &&
	r=$(web_get e2einet --resolve nodo.local:80:203.0.113.2 http://nodo.local/); echo "$r"; [ "$r" != 200 ] &&
	! grep -q "Trust this device" /tmp/e2e.body'
check "the trust files are readable by Caddy and root only" sh -c '
	[ "$(stat -c "%a %U %G" /var/lib/nodo-web/trust)" = "750 root _caddy" ] &&
	[ "$(stat -c "%a %U %G" /var/lib/nodo-web/trust/nodo-local-ca.crt)" = "640 root _caddy" ]'

# A new local address: nodo-localnet tells the helper, which moves the listener, the firewall, the
# mDNS answers and the trust page with no exposure change.
mark=$(wc -l < /var/log/nodo-helper/current)
printf 'MODE=static\nPARENT=e2elan0\nADDRESS=192.168.77.4/24\n' > /etc/sv/nodo-localnet/conf
sv -w 20 restart nodo-localnet >/dev/null 2>&1
check "a new local address reaches Caddy and the firewall, told by nodo-localnet" sh -c '
	. /tmp/e2e-web-lib.sh; until_ok 20 applied 192.168.77.4 &&
	tail -n "+$(($0 + 1))" /var/log/nodo-helper/current | grep -q "uid 0 local-address-changed: ok" &&
	tail -n "+$(($0 + 1))" /var/log/nodo-helper/current | grep -q "local address .* -> 192.168.77.4" &&
	grep -q "\"192.168.77.4:10444\"" /var/lib/nodo-web/caddy.json && ! grep -q 192.168.77.3 /var/lib/nodo-web/caddy.json &&
	nft list ruleset | grep -q "dnat ip to 192.168.77.4:10444" && nft list ruleset | grep -q "dnat ip to 192.168.77.4:10081" &&
	! nft list ruleset | grep -q 192.168.77.3' "$mark"
check "the local site and the trust page answer on the new address" sh -c '
	. /tmp/e2e-web-lib.sh
	is "$(web_get e2elan --resolve shop.local:443:192.168.77.4 https://shop.local/admin)" 200 && grep -q "e2e shop /admin" /tmp/e2e.body &&
	is "$(web_get e2elan --resolve nodo.local:80:192.168.77.4 http://nodo.local/)" 200 && grep -q "Trust this device" /tmp/e2e.body'
check "mDNS answers both names with the new address" sh -c '
	. /tmp/e2e-web-lib.sh; until_ok 10 mdns_says shop.local 192.168.77.4 && mdns_says nodo.local 192.168.77.4'
check "the old address no longer answers" sh -c '
	. /tmp/e2e-web-lib.sh; ! mdns_says shop.local 192.168.77.3 &&
	r=$(web_get e2elan -k --resolve shop.local:443:192.168.77.3 https://shop.local/); echo "$r"; [ "${r#ERR}" != "$r" ] &&
	r=$(web_get e2elan --resolve nodo.local:80:192.168.77.3 http://nodo.local/); echo "$r"; [ "${r#ERR}" != "$r" ]'
check "the new address is closed to the internet too" sh -c '
	. /tmp/e2e-web-lib.sh; is "$(web_get e2einet --resolve shop.local:443:192.168.77.4 https://shop.local/)" ERR28 &&
	is "$(web_get e2einet --resolve nodo.local:80:192.168.77.4 http://nodo.local/)" ERR28'

# A change the helper was not told about, here because it was stopped, is caught by its own check
# within a minute. It starts locked, as after a reboot, so the unlock cannot be what applies it.
sv -w 30 stop nodo-helper >/dev/null
printf 'MODE=static\nPARENT=e2elan0\nADDRESS=192.168.77.3/24\n' > /etc/sv/nodo-localnet/conf
sv -w 20 restart nodo-localnet >/dev/null 2>&1
sleep 3
sv -w 30 start nodo-helper >/dev/null
wait_socket
check "the helper's own check applies a change it missed" sh -c '
	. /tmp/e2e-web-lib.sh; until_ok 90 applied 192.168.77.3 &&
	grep -q "\"192.168.77.3:10444\"" /var/lib/nodo-web/caddy.json && nft list ruleset | grep -q "dnat ip to 192.168.77.3:10444" &&
	until_ok 10 mdns_says shop.local 192.168.77.3'
printf '%s\n' "$PASS2" | nodo-tui --ctl unlock >/dev/null
check "dynamic DNS stays off while nothing is public" sh -c '! sv status nodo-ddns 2>/dev/null | grep -q "^run:"'

if [ -d /etc/sv/tor ]; then
	check "onion exposure starts Tor and serves the onion address" sh -c '
		ctl expose e2e-app shop 1 1 0 && ctl web-status | tr -d " \n" | tee /tmp/e2e.ws | grep -q "onion_url:Some(\"http://"'
	onion_host=$(grep -o 'onion_url:Some("http://[a-z2-7]*\.onion"' /tmp/e2e.ws | head -1 | sed 's|.*http://||; s|"$||')
	check "the onion listener answers for the onion host only" sh -c '
		. /tmp/e2e-web-lib.sh; is "$(web_get host -H "Host: $0" http://127.0.0.1:19000/)" 200 &&
		grep -q "e2e shop /" /tmp/e2e.body &&
		is "$(web_get host -H "Host: shop.local" http://127.0.0.1:19000/)" ERR52 &&
		is "$(web_get host -H "Host: nodo.local" http://127.0.0.1:19000/)" ERR52' "$onion_host"
else
	echo "SKIP onion: tor is not installed"
	ctl expose e2e-app shop 1 0 0 >/dev/null
fi

check "an unknown DNS provider is refused" refused "not a supported provider" sh -c 'echo x | nodo-tui --ctl set-dns example.com namecheap api_key'
# The device was unlocked above, so the credentials can be sealed.
check "DNS credentials are stored sealed and root only" sh -c '
	echo cfut_0123456789abcdefghijABCDEFGHIJ0123456789 | nodo-tui --ctl set-dns example.com cloudflare api_token &&
	[ "$(stat -c "%a %U" /var/lib/nodo/secrets/web-dns.toml)" = "600 root" ] && ! grep -q cfut_ /var/lib/nodo/secrets/web-dns.toml &&
	grep -q "api_token = \"v1\." /var/lib/nodo/secrets/web-dns.toml && ! ctl web-status | grep -q cfut_'
check "the provider list carries DNS-01 support" sh -c 'ctl dns-providers | grep -A12 "id: \"godaddy\"" | grep -q BundledSeparateCredentials'
check "going public without the typed host is refused" refused "NeedsConfirmation" ctl expose e2e-app shop 1 0 1
check "going public with the wrong host is refused" refused "NeedsConfirmation" ctl expose e2e-app shop 1 0 1 evil.example.com
check "refused attempts left the firewall closed" sh -c '! nft list ruleset | grep -q "redirect to :10443"'
check "going public with the typed host works" ctl expose e2e-app shop 1 0 1 shop.example.com
check "the site lists as public" sh -c 'ctl sites | grep -A6 "site: \"shop\"" | grep -q "public: true"'
check "the public ports are redirected to Caddy" sh -c 'nft list ruleset | grep -q "redirect to :10443" && nft list ruleset | grep -q "redirect to :10080"'
# The VM is offline, so no public certificate can be issued. Port 80 shows the public listener routes.
check "port 80 on the main address redirects the public host to https" sh -c '
	. /tmp/e2e-web-lib.sh; r=$(web_get e2einet -D /tmp/e2e.h --resolve shop.example.com:80:203.0.113.2 http://shop.example.com/x);
	is "$r" 308 && grep -qi "^location: https://shop.example.com/x" /tmp/e2e.h'
check "neither the public listener nor the main address serves the trust page" sh -c '
	. /tmp/e2e-web-lib.sh
	r=$(web_get e2einet --resolve nodo.local:80:203.0.113.2 http://nodo.local/); echo "$r"; [ "$r" != 200 ] &&
	! grep -q "Trust this device" /tmp/e2e.body &&
	r=$(web_get e2elan --resolve nodo.local:80:192.168.77.2 http://nodo.local/); echo "$r"; [ "$r" != 200 ] &&
	! grep -q "Trust this device" /tmp/e2e.body'
check "nodo-ddns runs while the site is public, with a private config" sh -c '
	sv status nodo-ddns | grep -q "^run:" && [ "$(stat -c "%a %U" /run/nodo/secrets/nodo-ddns/config.toml)" = "600 _nododdns" ] &&
	grep -q "records = \[\"shop\"\]" /run/nodo/secrets/nodo-ddns/config.toml'
check "web status reports the addresses and dynamic DNS" sh -c '
	ctl web-status | tr -d " \n" > /tmp/e2e.ws; cat /tmp/e2e.ws; grep -q "local_address:Some(\"192.168.77.3\"" /tmp/e2e.ws &&
	grep -q "public_url:Some(\"https://shop.example.com\"" /tmp/e2e.ws && grep -q "ddns_running:true" /tmp/e2e.ws &&
	grep -q "rate_limit_unenforced:false" /tmp/e2e.ws'
check "the reachability check runs as root and never claims the ports" sh -c '
	ctl reach e2e-app shop | tee /tmp/e2e.r; grep -q "verdict:" /tmp/e2e.r && ! grep -q "verdict: \"ready\"" /tmp/e2e.r &&
	! grep -B2 -A2 "port_443" /tmp/e2e.r | grep -q "status: Passed"'
check "retiring stale records is an explicit action" sh -c 'ctl retire-stale | grep -q RetiredRecords'
check "taking the site off public stops dynamic DNS and closes the ports" sh -c '
	ctl expose e2e-app shop 1 0 0 && ! sv status nodo-ddns | grep -q "^run:" && ! nft list ruleset | grep -q "redirect to :10443"'
check "port 80 is closed again" sh -c '
	. /tmp/e2e-web-lib.sh; is "$(web_get e2einet --resolve shop.example.com:80:203.0.113.2 http://shop.example.com/)" ERR28'
printf 'MODE=off\n' > /etc/sv/nodo-localnet/conf
sv -w 20 restart nodo-localnet >/dev/null 2>&1
check "without a local address, local sites wait with a reason and nothing falls back" sh -c '
	ctl expose e2e-app shop 1 0 0; ctl web-status | tr -d " \n" > /tmp/e2e.ws; cat /tmp/e2e.ws
	grep -q "local_unavailable:Some(\"Localsitesareswitchedoff.\"" /tmp/e2e.ws &&
	grep -q "local_waiting:true" /tmp/e2e.ws && ! grep -q shop.local /var/lib/nodo-web/caddy.json'
check "turning every exposure off stops Caddy and removes the trust page" sh -c '
	ctl expose e2e-app shop 0 0 0 && ! sv status nodo-caddy | grep -q "^run:" && [ ! -e /var/lib/nodo-web/trust/index.html ]'
check "a single-address site refuses two exposures" refused "single address" ctl expose e2e-app admin 1 1 0
check "a single-address site takes one" ctl expose e2e-app admin 0 1 0
ctl expose e2e-app admin 0 0 0 >/dev/null
web_down

# Certificates from a real ACME server, Pebble, through every challenge
if [ -f /tmp/acme-e2e.sh ]; then
	. /tmp/acme-e2e.sh
else
	echo "SKIP acme: /tmp/acme-e2e.sh is not in the VM"
fi

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

loopdev=$(losetup -j /var/tmp/e2e-drive-640.img | cut -d: -f1)
if [ -z "$loopdev" ]; then
	rm -f /var/tmp/e2e-drive-640.img
	truncate -s 640M /var/tmp/e2e-drive-640.img
	loopdev=$(losetup -f --show /var/tmp/e2e-drive-640.img)
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

# An encrypted package with its volume on the data drive, and its settings inside the volume

mkdir -p /opt/nodo/apps/e2e-vault2/bin
cp /opt/nodo/apps/e2e-vault/bin/run /opt/nodo/apps/e2e-vault2/bin/run
sed 's/e2e-vault/e2e-vault2/' /var/lib/nodo/manifests/e2e-vault.toml | sed 's/^exec = .*/&\nsettings = "settings.lua"/' > /var/lib/nodo/manifests/e2e-vault2.toml
cat > /opt/nodo/apps/e2e-vault2/settings.lua <<'EOF'
return { api_version = 1, settings = {
  { name = "motto", type = "string", label = "Motto", default = "kept in the volume" },
} }
EOF
printf '%s\n' "$PASS2" | nodo-tui --ctl unlock >/dev/null
check "a package can be given the data drive before it is installed" ctl data-drive e2e-vault2 1
check "its encrypted volume is created on the data drive, not the system disk" sh -c '
	printf "%s\n" "$0" | nodo-tui --ctl install-encrypted e2e-vault2 &&
	cryptsetup isLuks --type luks2 /media/e2e/.nodo-volumes/e2e-vault2.luks &&
	[ ! -e /var/lib/nodo/volumes/e2e-vault2.luks ] && mountpoint -q /run/nodo/volumes/e2e-vault2' "$PASS2"
check "its rendered settings and logs are inside the volume" sh -c '
	grep -q "kept in the volume" /run/nodo/volumes/e2e-vault2/config/settings.toml &&
	[ ! -e /etc/apps/e2e-vault2 ] && [ ! -e /var/log/apps/e2e-vault2 ] &&
	! grep -rqs "kept in the volume" /etc /var/lib /var/log /media/e2e'
check "it runs from the volume on the data drive" running_for e2e-vault2 3
check "the data drive cannot be chosen after install" refused "already installed" ctl data-drive e2e-vault2 0
lock_volume e2e-vault2
umount /media/e2e
check "without the drive, unlock waits for it rather than looking elsewhere" \
	refused "waiting for the data drive at /media/e2e" sh -c 'printf "%s\n" "$0" | nodo-tui --ctl unlock' "$PASS2"
check "nothing was created underneath the empty mount point" sh -c '[ ! -e /media/e2e/.nodo-volumes ]'

# Feeds and installs: a feed signed in the VM with a real RSA key, published with xbps-rindex

# The data drive comes back, so every volume is open again for what follows.
mount /media/e2e
printf '%s\n' "$PASS2" | nodo-tui --ctl unlock >/dev/null

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
# The current version and the one before it, which a rollback by hand puts back. Never the failed one.
check "the current and the previous version's copies are kept, nothing else" sh -c '
	[ "$(ls /var/lib/nodo/packages/e2e-pay | tr "\n" " ")" = "e2e-pay-1.0_1.x86_64.xbps e2e-pay-1.3_1.x86_64.xbps " ]'
check "the previous version is offered as a rollback" sh -c '
	ctl rollbacks | tee /tmp/e2e.rbl; grep -q "current: \"1.3_1\"" /tmp/e2e.rbl && grep -q "previous: \"1.0_1\"" /tmp/e2e.rbl &&
	grep -q "blocked: None" /tmp/e2e.rbl'
check "a rollback naming another version is refused and changes nothing" sh -c '
	ctl rollback e2e-pay 1.2_1 2>&1 | grep -q NeedsConfirmation && [ "$(xbps-query -p pkgver e2e-pay)" = e2e-pay-1.3_1 ]'
check "the auto-update timer service asks the helper" sh -c '
	[ -L /var/service/nodo-autoupdate ] || ln -s /etc/sv/nodo-autoupdate /var/service/
	for _ in $(seq 1 20); do grep -q UpdateResults /var/log/nodo-autoupdate/current 2>/dev/null && exit 0; sleep 1; done; exit 1'
rm -f /var/service/nodo-autoupdate

# Encrypted storage across updates: the volume is kept, grows when a version asks for more, the
# data layout is recorded, and a failed update puts the old data back from the copy in the volume.

# Builds e2e-store, an encrypted app, at a version with extra [storage] lines and a start step.
mkstore() {
	ver=$1 storage=$2 start=$3
	d=$APPROOT/e2e-store-$ver
	rm -rf "$d"
	mkdir -p "$d/opt/nodo/apps/e2e-store/bin"
	chmod 755 "$d" "$d/opt" "$d/opt/nodo" "$d/opt/nodo/apps" "$d/opt/nodo/apps/e2e-store" "$d/opt/nodo/apps/e2e-store/bin"
	printf 'api_version = 1\nid = "e2e-store"\nversion = "%s"\nexec = ["bin/run"]\nstop_timeout_secs = 5\n\n[storage]\nencrypted = true\n%s\n' \
		"$ver" "$storage" > "$d/opt/nodo/apps/e2e-store/manifest.toml"
	printf '#!/bin/sh\nD=/var/lib/apps/e2e-store\n%s\necho "e2e-store %s running"\nwhile :; do sleep 1; done\n' "$start" "$ver" \
		> "$d/opt/nodo/apps/e2e-store/bin/run"
	chmod 644 "$d/opt/nodo/apps/e2e-store/manifest.toml"
	chmod 755 "$d/opt/nodo/apps/e2e-store/bin/run"
	(cd "$APPROOT" && xbps-create -A x86_64 -n "e2e-store-${ver}_1" -s "e2e encrypted app" "$d" >/dev/null)
	publish "$APPROOT/e2e-store-${ver}_1.x86_64.xbps"
}
STORE=/run/nodo/volumes/e2e-store
mkstore 1.0 '' '[ -e "$D/notes.txt" ] || echo "first note" > "$D/notes.txt"'
ctl refresh-feeds >/dev/null
check "an encrypted feed package installs with the device passphrase" sh -c '
	printf "%s\n" "$0" | nodo-tui --ctl approve-install-encrypted e2e-feed e2e-store | tee /tmp/e2e.si; grep -q "problem: None" /tmp/e2e.si' "$PASS2"
check "its volume has the default size and records data layout 1" sh -c '
	[ "$(stat -c %s /var/lib/nodo/volumes/e2e-store.luks)" = 268435456 ] && [ "$(cat '"$STORE"'/.nodo-layout)" = 1 ] &&
	[ "$(stat -c "%a %U" '"$STORE"'/.nodo-layout)" = "644 root" ]'
check "version 1.0 writes its data in layout 1" sh -c '
	for _ in $(seq 1 20); do [ -f '"$STORE"'/data/notes.txt ] && exit 0; sleep 1; done; exit 1'
echo user-data > "$STORE/data/user.txt" && chown _e2e-store: "$STORE/data/user.txt"
uuid=$(cryptsetup luksUUID /var/lib/nodo/volumes/e2e-store.luks)
fs_before=$(df -k --output=size "$STORE" | tail -n 1)
mkstore 2.0 'size_mib = 320
data_layout = 2' 'if [ -e "$D/notes.txt" ]; then { echo "format 2"; cat "$D/notes.txt"; } > "$D/notes.new" && mv "$D/notes.new" "$D/notes.v2" && rm "$D/notes.txt"; fi'
ctl auto-set daily 00:00 23:59 e2e-pay e2e-store >/dev/null
check "an update asking for a larger volume waits for the device passphrase" sh -c '
	ctl check-updates | tee /tmp/e2e.st; grep -q "needs the device passphrase to grow its encrypted storage" /tmp/e2e.st'
check "applying it without the passphrase is refused and changes nothing" sh -c '
	ctl apply-update e2e-store 2.0_1 | tee /tmp/e2e.st2; grep -q "Refused" /tmp/e2e.st2 && grep -q passphrase /tmp/e2e.st2 &&
	[ "$(xbps-query -p pkgver e2e-store)" = e2e-store-1.0_1 ] && [ "$(stat -c %s /var/lib/nodo/volumes/e2e-store.luks)" = 268435456 ]'
check "with the passphrase the volume grows in place and 2.0 is installed on it" sh -c '
	printf "%s\n" "$0" | nodo-tui --ctl apply-update-pass e2e-store 2.0_1 | tee /tmp/e2e.st3; grep -q Updated /tmp/e2e.st3 &&
	[ "$(xbps-query -p pkgver e2e-store)" = e2e-store-2.0_1 ]' "$PASS2"
check "it is the same LUKS volume, larger, with its filesystem grown while mounted" sh -c '
	cryptsetup luksUUID /var/lib/nodo/volumes/e2e-store.luks; stat -c %s /var/lib/nodo/volumes/e2e-store.luks; df -k '"$STORE"'; echo "before: $1"
	[ "$(cryptsetup luksUUID /var/lib/nodo/volumes/e2e-store.luks)" = "$0" ] &&
	[ "$(stat -c %s /var/lib/nodo/volumes/e2e-store.luks)" = 335544320 ] &&
	[ "$(df -k --output=size '"$STORE"' | tail -n 1)" -gt "$(($1 + 50000))" ] && mountpoint -q '"$STORE"'' "$uuid" "$fs_before"
check "2.0 converted the data to layout 2, kept the rest, and the layout is recorded" sh -c '
	for _ in $(seq 1 20); do [ -f '"$STORE"'/data/notes.v2 ] && break; sleep 1; done
	grep -q "first note" '"$STORE"'/data/notes.v2 && [ ! -e '"$STORE"'/data/notes.txt ] &&
	[ "$(cat '"$STORE"'/data/user.txt)" = user-data ] && [ "$(cat '"$STORE"'/.nodo-layout)" = 2 ]'
check "no copy of the old data is left behind" sh -c '[ ! -e '"$STORE"'/.nodo-rollback ]'
check "2.0 runs on the kept volume" running_for e2e-store 3
sum=$(cat "$STORE/data/notes.v2" "$STORE/data/user.txt" | sha256sum)
mkstore 3.0 'data_layout = 3' 'rm -f "$D/notes.v2" "$D/user.txt"; echo "format 3, half done" > "$D/notes.v3"; echo "migration to layout 3 failed" >&2; exit 1'
check "an update to layout 3 that breaks its data and fails its health check rolls back" sh -c '
	ctl check-updates >/dev/null && ctl apply-update e2e-store 3.0_1 | tee /tmp/e2e.st4; grep -q RolledBack /tmp/e2e.st4 &&
	grep -q "recovered: true" /tmp/e2e.st4'
check "the data is back exactly as 2.0 left it, still in layout 2" sh -c '
	[ "$(cat '"$STORE"'/data/notes.v2 '"$STORE"'/data/user.txt | sha256sum)" = "$0" ] && [ ! -e '"$STORE"'/data/notes.v3 ] &&
	[ "$(cat '"$STORE"'/.nodo-layout)" = 2 ] && [ ! -e '"$STORE"'/.nodo-rollback ] &&
	[ "$(xbps-query -p pkgver e2e-store)" = e2e-store-2.0_1 ] &&
	[ "$(cryptsetup luksUUID /var/lib/nodo/volumes/e2e-store.luks)" = "$1" ]' "$sum" "$uuid"
check "2.0 runs again after the rollback" running_for e2e-store 3
mkstore 4.0 '' 'true'
check "a version declaring an older data layout is refused before anything changes" sh -c '
	ctl check-updates >/dev/null && ctl apply-update e2e-store 4.0_1 | tee /tmp/e2e.st5; grep -q "data layout 1, older" /tmp/e2e.st5 &&
	[ "$(xbps-query -p pkgver e2e-store)" = e2e-store-2.0_1 ] && [ "$(cat '"$STORE"'/data/notes.v2 '"$STORE"'/data/user.txt | sha256sum)" = "$0" ]' "$sum"
check "2.0 still runs" running_for e2e-store 3
ctl auto-set daily 00:00 23:59 e2e-pay >/dev/null

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
		[ -L /var/service/monokulo ] && grep -q "encryption_key" /var/lib/nodo/settings/monokulo.toml && ! grep -q "encryption_key = \"[0-9a-f]" /var/lib/nodo/settings/monokulo.toml' "$PASS2"
	# The VM is offline and its node has no chain, so Monokulo may not pass its health check here.
	# Either way the install must report the result rather than claim success.
	check "Monokulo's health check result is reported, not hidden" sh -c '
		grep -q "problem: None" /tmp/e2e.mki || grep -q "did not pass its health check" /tmp/e2e.mki'
	sv down monokulo
fi

# Backups and restore with the device key

mkdir -p /var/lib/nodo/secrets && echo keep-me > /var/lib/nodo/secrets/e2e
echo vault-data > /run/nodo/volumes/e2e-vault/data/e2e-file
chown _e2e-vault: /run/nodo/volumes/e2e-vault/data/e2e-file
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
rm -f /var/lib/nodo/secrets/e2e /run/nodo/volumes/e2e-vault/data/e2e-file
# The sealed DNS credentials from the web checks live in the secrets directory, so backups carry them.
cp /var/lib/nodo/secrets/web-dns.toml /tmp/e2e-dns.before && rm -f /var/lib/nodo/secrets/web-dns.toml
check "a tampered backup is refused and changes nothing" refused "altered\\|damaged" sh -c '
	printf "%s\n%s\n" "$0" "new device pass" | nodo-tui --ctl restore nodo-tampered.nodobak restore' "$BACKUP_PASS"
check "a wrong passphrase restores nothing" refused "passphrase did not open" sh -c '
	printf "wrong backup passphrase\n%s\n" "new device pass" | nodo-tui --ctl restore "$0" restore' "$bk"
check "a restore without the typed word is refused" refused "NeedsConfirmation" sh -c '
	printf "%s\n%s\n" "$1" "new device pass" | nodo-tui --ctl restore "$0" yes' "$bk" "$BACKUP_PASS"
check "the refused restores changed nothing" sh -c '
	[ ! -e /var/lib/nodo/secrets/e2e ] && grep -qx "greeting = \"after\"" /run/nodo/secrets/e2e-app/config/settings.toml && ! ls -d /var/lib/nodo/.restore-* 2>/dev/null'
check "a restore stops the services it replaces, restores and starts them" sh -c '
	printf "%s\n%s\n" "$1" "new device pass" | nodo-tui --ctl restore "$0" restore | tee /tmp/e2e.rs
	sed -n "/stopped:/,/\]/p" /tmp/e2e.rs | grep -q "\"e2e-vault\"" && sed -n "/restarted:/,/\]/p" /tmp/e2e.rs | grep -q "\"e2e-vault\""' "$bk" "$BACKUP_PASS"
check "settings, secrets and storage are back" sh -c '
	grep -qx "greeting = \"before\"" /run/nodo/secrets/e2e-app/config/settings.toml && [ "$(cat /var/lib/nodo/secrets/e2e)" = keep-me ] &&
	[ "$(cat /run/nodo/volumes/e2e-vault/data/e2e-file)" = vault-data ] && [ "$(stat -c %U /run/nodo/volumes/e2e-vault/data/e2e-file)" = _e2e-vault ]'
check "the device key used by this device's volumes was kept" sh -c '
	grep -q "uses its own key" /tmp/e2e.rs && printf "%s\n" "$0" | nodo-tui --ctl unlock' "$PASS2"
check "the restored services run" running_for e2e-vault 3
check "sealed DNS credentials come back from the backup and still open" sh -c '
	cmp /var/lib/nodo/secrets/web-dns.toml /tmp/e2e-dns.before &&
	[ "$(stat -c "%a %U" /var/lib/nodo/secrets/web-dns.toml)" = "600 root" ] &&
	ctl expose e2e-app shop 0 0 1 shop.example.com && grep -q "api_token = \"cfut_0123" /run/nodo/secrets/nodo-ddns/config.toml'
ctl expose e2e-app shop 0 0 0 >/dev/null
for s in nodo-caddy nodo-ddns; do [ -d "/etc/sv/$s" ] && { touch "/etc/sv/$s/down"; sv -w 15 down "$s" >/dev/null 2>&1; }; done
nft -f /etc/nftables.conf
check "no staging copy is left behind" sh -c '! ls -d /var/lib/nodo/.restore-* 2>/dev/null'

# The real TUI against the real helper: feeds, packages, updates, Home's attention and backups

rm -f /tmp/e2e-tui.out
(
	sleep 4; printf '\t'; sleep 3; printf '\t'; sleep 2; printf '\t'; sleep 3
	printf '\t'; sleep 3; printf '\t'; sleep 2; printf '\t'; sleep 3
	printf 'b'; sleep 3; printf '\033'; sleep 2; printf 'q'; sleep 1
) | script -qfec 'stty rows 32 cols 100; TERM=xterm-256color nodo-tui' /tmp/e2e-tui.out >/dev/null 2>&1
# Unchanged cells are skipped with cursor moves, so spaces are removed before comparing.
sed 's/\x1b\[[0-9;?]*[A-Za-z]//g; s/\x1b[()][A-Z0-9]//g' /tmp/e2e-tui.out | tr -d ' ' > /tmp/e2e-tui.txt
for want in "Your feeds" "e2e-feed" "e2e-pay" "Backups on this device" "$bk" "is back and the update is on hold"; do
	check "the TUI showed: $want" grep -q "$(printf %s "$want" | tr -d ' ')" /tmp/e2e-tui.txt
done
check "the TUI no longer says feeds or updates are not available" sh -c '! grep -q "Not available yet: feeds\|Not available yet: automatic" /tmp/e2e-tui.txt'
check "the TUI never showed a passphrase" sh -c '! grep -q "horse" /tmp/e2e-tui.txt'

# Health: the daemon's configuration from manifests, and a low disk on a small loop filesystem

cp /etc/sv/nodo-helper/conf /tmp/e2e-helper.conf
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
rm -f /run/nodo/boot-fallback
# Abstract Unix socket listeners: none expected, and an unprivileged one is named on Home.
if [ -f /tmp/abstract-sockets.sh ]; then
	. /tmp/abstract-sockets.sh
else
	echo "SKIP abstract socket listeners: copy tests/abstract-sockets.sh to /tmp/abstract-sockets.sh"
fi
rm -f /var/service/nodo-healthd
sleep 2
umount /media/e2e-health
cp /tmp/e2e-helper.conf /etc/sv/nodo-helper/conf
restart_helper
# A helper restart forgets the key that opens sealed secrets, as a reboot would.
printf '%s\n' "$PASS2" | nodo-tui --ctl unlock >/dev/null

# Settings rollback, last because it waits for the crash loop to be noticed

check "a setting that makes the app crash is rolled back" sh -c '
	ctl set e2e-app mode text crash | tee /tmp/e2e.rb; grep -q RolledBack /tmp/e2e.rb && grep -q "recovered: true" /tmp/e2e.rb &&
	! grep -q crash "$0"' "$APPCONF"
check "the app runs again on its old settings" running_for e2e-app 3

rm -f /etc/nodo/acme-test.toml
echo "helper log tail:"
tail -n 5 /var/log/nodo-helper/current
exit "$fail"
