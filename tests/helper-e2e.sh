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

# Settings rollback, last because it waits for the crash loop to be noticed

check "a setting that makes the app crash is rolled back" sh -c '
	ctl set e2e-app mode text crash | tee /tmp/e2e.rb; grep -q RolledBack /tmp/e2e.rb && grep -q "recovered: true" /tmp/e2e.rb &&
	! grep -q crash "$0"' "$APPCONF"
check "the app runs again on its old settings" running_for e2e-app 3

echo "helper log tail:"
tail -n 5 /var/log/nodo-helper/current
exit "$fail"
