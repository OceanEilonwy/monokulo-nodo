#!/bin/sh
# Runs on the device under test, as root, after tests/snapshot-prepare.sh put test snapshots in
# /srv/snapshot-test. Needs nodo-tools (nodo-sysupdate) and nodo-caddy. A fake update server, Caddy
# in a network namespace on an internet-like address, serves them over HTTPS with a throwaway CA
# and over plain HTTP, and the real client moves the system between them:
#
#   it applies a signed snapshot over HTTP, refuses an unsigned, tampered, foreign or expired one,
#   a tampered index and a tampered package, applies the next one over HTTPS so that a service
#   updates and comes back, and then refuses the older snapshot and a conflicting one. Then,
#   through the helper as the Updates screen uses it and through the nodo-systemupdate schedule,
#   it refuses a newer snapshot that would move a package back without withdrawing anything,
#   leaves a withdrawal alone while automatic system updates are off, and applies it when the
#   owner asks, so that the service goes back to the old version. Last, with automatic system
#   updates turned on, a run leaves a snapshot waiting outside the package update window, the
#   schedule applies it by itself inside it, and a snapshot that needs a restart still waits.
#
# The VM check runs it whenever tests/snapshot-prepare.sh has put snapshots in the VM, after
# pushing this project's packages so the VM holds the snapshot's versions of them. It can run
# again on the same VM. By hand:
#   scripts/dev-vm.sh ssh 'sh /tmp/snapshot-update.sh </dev/null'
set -u
ns=updinet
peer=198.18.0.1
src=/srv/snapshot-test
www=$src/server
pins=/usr/share/nodo/snapshot-pins.toml
state=/var/lib/nodo/update/system-trust.toml
fail=0
result() { if [ "$2" = 0 ]; then echo "PASS $1"; else echo "FAIL $1"; fail=1; fi; }
# Succeeds when the command's output contains the text.
says() { text=$1; shift; "$@" > /tmp/su.out 2>&1; grep -q -- "$text" /tmp/su.out || { sed 's/^/    /' /tmp/su.out | head -n 15; return 1; }; }
update() { nodo-sysupdate "$@"; }
https=https://updates.example.test
http=http://updates.example.test
version() { xbps-query -p pkgver nodo-snaptest; }
ctl() { nodo-tui --ctl "$@"; }
helper_up() { for _ in $(seq 1 30); do [ -S /run/nodo/helper.sock ] && ctl services >/dev/null 2>&1 && return 0; sleep 1; done; return 1; }
serial() { sed -n 's/^serial = //p' "$state" 2>/dev/null; }
# Makes snapshot $1's manifest, or the attacker's, the channel's newest.
offer() {
	mkdir -p "$www/channels/test"
	rm -f "$www/channels/test/manifest.toml" "$www/channels/test/manifest.toml.sig"
	case $1 in
	attacker) cp "$www/attacker/manifest.toml" "$www/attacker/manifest.toml.sig" "$www/channels/test/" ;;
	*) cp "$www/snapshots/$1/manifest.toml" "$www/snapshots/$1/manifest.toml.sig" "$www/channels/test/" ;;
	esac
}
unchanged() { [ "$(version)" = "$1" ] && [ "$(serial)" = "$2" ]; }

cleanup() {
	sv force-stop nodo-systemupdate >/dev/null 2>&1
	rm -f /var/service/nodo-systemupdate /etc/nodo/system-update.toml /var/service/nodo-snaptest
	ip netns pids "$ns" 2>/dev/null | xargs -r kill 2>/dev/null
	ip netns del "$ns" 2>/dev/null
	ip link del upd0 2>/dev/null
	sed -i '/# nodo-update-test$/d' /etc/hosts
	rm -f /usr/local/share/ca-certificates/nodo-update-test.crt
	update-ca-certificates >/dev/null 2>&1
}
trap cleanup EXIT
[ -s "$www/snapshots/test-b/manifest.toml.sig" ] || { echo "FAIL run tests/snapshot-prepare.sh first"; exit 1; }

ip netns add "$ns"
ip link add upd0 type veth peer name eth0 netns "$ns"
ip addr add 198.18.0.2/24 dev upd0
ip link set upd0 up
ip netns exec "$ns" ip addr add "$peer/24" dev eth0
ip netns exec "$ns" ip link set lo up
ip netns exec "$ns" ip link set eth0 up
mkdir -p "$src/caddy"
cat > "$src/caddy/config.json" <<JSON
{"admin":{"disabled":true},"logging":{"logs":{"default":{"level":"ERROR"}}},
 "apps":{
  "tls":{"automation":{"policies":[{"issuers":[{"module":"internal"}]}]}},
  "pki":{"certificate_authorities":{"local":{"install_trust":false}}},
  "http":{"servers":{
   "tls":{"listen":["$peer:443"],"automatic_https":{"disable_redirects":true},"routes":[
    {"match":[{"host":["updates.example.test"]}],"handle":[{"handler":"file_server","root":"$www"}]}]},
   "plain":{"listen":["$peer:80"],"automatic_https":{"disable":true},"routes":[
    {"match":[{"host":["updates.example.test"]}],"handle":[{"handler":"file_server","root":"$www"}]}]}}}}}
JSON
XDG_DATA_HOME=$src/caddy XDG_CONFIG_HOME=$src/caddy HOME=$src/caddy \
	nohup ip netns exec "$ns" nodo-caddy run --config "$src/caddy/config.json" >"$src/caddy/caddy.log" 2>&1 </dev/null &
i=0; until [ -s "$src/caddy/caddy/pki/authorities/local/root.crt" ] || [ "$i" -ge 20 ]; do i=$((i+1)); sleep 1; done
mkdir -p /usr/local/share/ca-certificates
cp "$src/caddy/caddy/pki/authorities/local/root.crt" /usr/local/share/ca-certificates/nodo-update-test.crt
update-ca-certificates >/dev/null 2>&1
sed -i '/# nodo-update-test$/d' /etc/hosts
echo "$peer updates.example.test # nodo-update-test" >> /etc/hosts
sleep 2

# The device as an image would ship it: trusted keys with a floor, and nodo-snaptest 1.0 running.
# What an earlier run left goes first, including xbps's cached copies, which would otherwise
# stand in for the tampered package below.
rm -f /var/service/nodo-snaptest /var/service/nodo-systemupdate /etc/nodo/system-update.toml /var/lib/nodo/updates/system.toml
xbps-query nodo-snaptest >/dev/null 2>&1 && xbps-remove -y nodo-snaptest >/dev/null
rm -f /var/cache/xbps/nodo-snaptest-*
install -D -m 644 "$src/pins.toml" "$pins"
rm -f "$state"
[ -L /var/service/nodo-helper ] || ln -s /etc/sv/nodo-helper /var/service/
helper_up
result "control: the helper answers" "$?"
xbps-rindex -a "$src"/local/*.xbps >/dev/null
xbps-install -y -R "$src/local" nodo-snaptest >/dev/null
ln -sfn /etc/sv/nodo-snaptest /var/service/nodo-snaptest
for _ in $(seq 1 20); do sv status nodo-snaptest 2>/dev/null | grep -q '^run:' && break; sleep 1; done
if [ "$(version)" = nodo-snaptest-1.0_1 ] && [ "$(cat /run/nodo-snaptest.version)" = 1.0 ]; then r=0; else r=1; fi
result "control: nodo-snaptest 1.0 is installed and running" "$r"
pid_before=$(sv status nodo-snaptest | sed -n 's/^run: [^ ]* (pid \([0-9]*\)).*/\1/p')

rm -rf "$www/channels"
says "has no channels/test/manifest.toml" update check --server "$https"
result "a server without the channel is reported plainly" "$?"

# Snapshot test-a, serial 5, over plain HTTP: signatures make that safe.
offer test-a
says "snapshot test-a (serial 5) is available" update check --server "$http" && [ ! -e "$state" ]
result "check finds the signed snapshot and records nothing" "$?"
says "applied snapshot test-a (serial 5)" update apply --server "$http" && [ "$(serial)" = 5 ]
result "a signed snapshot applies over plain HTTP and is recorded" "$?"
grep -l "monokulo-nodo" /var/db/xbps/keys/*.plist >/dev/null
result "the snapshot's xbps key was installed from its verified index" "$?"
xbps-query -L 2>/dev/null | grep -q "updates_example_test\|updates.example.test" || ls -d /var/db/xbps/http___updates_example_test_snapshots_test-a_* >/dev/null
result "xbps holds the verified indexes as its copy of the snapshot" "$?"
says "up to date" update apply --server "$https"
result "the same snapshot again is up to date" "$?"

# Refusals, each of which must change neither the system nor the record.
offer test-b
rm -f "$www/channels/test/manifest.toml.sig"
says "not signed" update apply --server "$https" && unchanged nodo-snaptest-1.0_1 5
result "an unsigned manifest is refused" "$?"
offer test-b
sed -i 's/^serial = 6$/serial = 60/' "$www/channels/test/manifest.toml"
says "not signed by enough trusted keys (0 of 1)" update apply --server "$https" && unchanged nodo-snaptest-1.0_1 5
result "a manifest changed after signing is refused" "$?"
offer attacker
says "signed by an unknown key" update apply --server "$https" && unchanged nodo-snaptest-1.0_1 5
result "a manifest signed by another key is refused" "$?"
offer test-b
says "expired" update apply --server "$https" --now 4102444800 && unchanged nodo-snaptest-1.0_1 5
result "an expired manifest is refused when the clock is known" "$?"
index=$www/snapshots/test-b/nodo/x86_64/x86_64-repodata
cp "$index" /tmp/su.index
printf 'X' | dd of="$index" bs=1 seek=100 conv=notrunc 2>/dev/null
says "does not match the signed manifest" update apply --server "$https" && unchanged nodo-snaptest-1.0_1 5
result "an index changed on the server is refused before xbps runs" "$?"
cp /tmp/su.index "$index"
pkg=$www/snapshots/test-b/nodo/x86_64/nodo-snaptest-1.1_1.x86_64.xbps
cp "$pkg" /tmp/su.pkg
printf 'X' | dd of="$pkg" bs=1 seek=300 conv=notrunc 2>/dev/null
says "xbps failed" update apply --server "$https" && unchanged nodo-snaptest-1.0_1 5
result "a package changed on the server is refused by xbps and nothing changes" "$?"
cp /tmp/su.pkg "$pkg"
rm -rf /var/cache/xbps/nodo-snaptest-1.1_1*

# Snapshot test-b, serial 6, over HTTPS: the service updates and comes back.
says "applied snapshot test-b (serial 6)" update apply --server "$https"
r=$?; cp /tmp/su.out /tmp/su.applied
if [ "$r" = 0 ] && [ "$(serial)" = 6 ] && [ "$(version)" = nodo-snaptest-1.1_1 ]; then r=0; else r=1; fi
result "the next snapshot applies over HTTPS and nodo-snaptest is 1.1" "$r"
grep -q "updated nodo-snaptest: 1.0_1 -> 1.1_1" /tmp/su.applied && grep -q "restarted nodo-snaptest" /tmp/su.applied
result "the update names what changed and which service restarted" "$?"
for _ in $(seq 1 20); do [ "$(cat /run/nodo-snaptest.version 2>/dev/null)" = 1.1 ] && break; sleep 1; done
pid_after=$(sv status nodo-snaptest | sed -n 's/^run: [^ ]* (pid \([0-9]*\)).*/\1/p')
if [ "$(cat /run/nodo-snaptest.version)" = 1.1 ] && [ -n "$pid_after" ] && [ "$pid_after" != "$pid_before" ]; then r=0; else r=1; fi
result "the service came back running the new version" "$r"
sv status nodo-helper | grep -q '^run:' && nodo-tui --ctl services >/dev/null
result "the helper still answers after the update" "$?"
! grep -q "updated nodo-tools\|updated nodo-caddy" /tmp/su.applied
result "packages already at the snapshot's versions were left alone" "$?"

# Rollback protection.
offer test-a
says "serial 5 is not newer than 6" update apply --server "$https" && unchanged nodo-snaptest-1.1_1 6
result "the older snapshot is refused" "$?"
offer test-c
says "a different manifest was already accepted with serial 6" update apply --server "$https" && unchanged nodo-snaptest-1.1_1 6
result "a different manifest with the same serial is refused" "$?"
# With the record lost, only the image's floor (serial 4) is left. An older snapshot above it is
# accepted, but xbps never moves a package back and the version pins then refuse to record it.
rm -f "$state"
offer test-a
says "nodo-snaptest is 1.1_1, but the snapshot says 1.0_1" update apply --server "$https" &&
	[ "$(version)" = nodo-snaptest-1.1_1 ] && [ ! -e "$state" ]
result "with the record lost, an older snapshot still cannot move packages back" "$?"
offer test-b
says "applied snapshot test-b (serial 6)" update apply --server "$https" && [ "$(serial)" = 6 ]
result "the current snapshot records itself again" "$?"

# Withdrawing test-b. A newer snapshot with older packages that withdraws nothing is refused
# before xbps runs, as it always was, but now with the reason.
offer test-e
says "nodo-snaptest is 1.1_1, but the snapshot says 1.0_1, and it does not withdraw" update apply --server "$https" &&
	unchanged nodo-snaptest-1.1_1 6
result "a newer snapshot that would move a package back without withdrawing anything is refused" "$?"

# The helper, as the Updates screen and the schedule use it. It reads the server from the config.
printf 'server = "%s"\n' "$https" > /etc/nodo/system-update.toml
offer test-d
says "automatic: false" ctl system-status
result "automatic system updates are off by default" "$?"
# Starts one scheduled run. After a link is removed its runsv can linger, stopped, and a new link
# then finds it there and starts nothing, so the run is asked for once runsv answers.
start_schedule() {
	ln -sfn /etc/sv/nodo-systemupdate /var/service/nodo-systemupdate
	i=0; until sv status nodo-systemupdate >/dev/null 2>&1 || [ "$i" -ge 20 ]; do i=$((i+1)); sleep 1; done
	sv restart nodo-systemupdate >/dev/null 2>&1
}
sv_pid() { sv status nodo-snaptest | sed -n 's/^run: [^ ]* (pid \([0-9]*\)).*/\1/p'; }
pid_before=$(sv_pid)
start_schedule
i=0; until grep -q '^serial = 8$' /var/lib/nodo/updates/system.toml 2>/dev/null || [ "$i" -ge 30 ]; do i=$((i+1)); sleep 1; done
grep -q '^withdraws_current = true$' /var/lib/nodo/updates/system.toml
result "the nodo-systemupdate schedule checked and found the withdrawal" "$?"
sleep 3
unchanged nodo-snaptest-1.1_1 6
result "with automatic system updates off, the schedule only reported it" "$?"
sv force-stop nodo-systemupdate >/dev/null 2>&1; rm -f /var/service/nodo-systemupdate
says "withdraws_current: true" ctl system-run && unchanged nodo-snaptest-1.1_1 6
result "a scheduled run applies nothing while automatic system updates are off" "$?"
says "withdraws_current: true" ctl system-check
result "checking through the helper says the snapshot withdraws this device's" "$?"
says "NeedsConfirmation" ctl system-apply 9 && unchanged nodo-snaptest-1.1_1 6
result "the helper applies only the serial the owner was shown" "$?"
says "SystemUpdateApplied" ctl system-apply 8
r=$?; cp /tmp/su.out /tmp/su.withdrawn
if [ "$r" = 0 ] && [ "$(serial)" = 8 ] && [ "$(version)" = nodo-snaptest-1.0_1 ]; then r=0; else r=1; fi
result "the owner applies the withdrawal through the helper and nodo-snaptest goes back to 1.0" "$r"
grep -A2 "withdrew: Some" /tmp/su.withdrawn | grep -q test-b && grep -q "nodo-snaptest: 1.1_1 -> 1.0_1" /tmp/su.withdrawn
result "the result names the withdrawn snapshot and the package that moved back" "$?"
for _ in $(seq 1 20); do [ "$(cat /run/nodo-snaptest.version 2>/dev/null)" = 1.0 ] && break; sleep 1; done
pid_after=$(sv_pid)
if [ "$(cat /run/nodo-snaptest.version)" = 1.0 ] && [ -n "$pid_after" ] && [ "$pid_after" != "$pid_before" ]; then r=0; else r=1; fi
result "the service came back running the old version" "$r"
says "available: None" ctl system-status && helper_up
result "the helper still answers and has nothing left to offer" "$?"
says "up to date" update apply --server "$https" && unchanged nodo-snaptest-1.0_1 8
result "the withdrawal is not applied twice" "$?"
offer test-b
says "serial 6 is not newer than 8" update apply --server "$https" && unchanged nodo-snaptest-1.0_1 8
result "the withdrawn snapshot cannot come back" "$?"

# Automatic system updates turned on: a scheduled run applies a snapshot that needs no restart,
# but only inside the package update window, and never one that needs a restart.
at() { date -d "@$(( $(date +%s) + $1 * 60 ))" +%H:%M; }
policy=/var/lib/nodo/updates/policy.toml
if [ -f "$policy" ]; then cp "$policy" /tmp/su.policy; else rm -f /tmp/su.policy; fi
says "Ok" ctl system-auto 1 && says "automatic: true" ctl system-status
result "automatic system updates turn on" "$?"
later=$(at 60) end=$(at 120)
ctl auto-set manual "$later" "$end" >/dev/null
offer test-f
says "serial: 9" ctl system-check && says "installs in the update window, from $later" ctl system-run &&
	unchanged nodo-snaptest-1.0_1 8
result "outside the update window, an automatic run leaves the snapshot waiting" "$?"
ctl auto-set manual "$(at -5)" "$(at 30)" >/dev/null
pid_before=$(sv_pid)
start_schedule
i=0; until [ "$(serial)" = 9 ] || [ "$i" -ge 120 ]; do i=$((i+1)); sleep 1; done
sv force-stop nodo-systemupdate >/dev/null 2>&1; rm -f /var/service/nodo-systemupdate
unchanged nodo-snaptest-1.1_1 9
result "inside the window, the nodo-systemupdate schedule applies it by itself" "$?"
ctl system-status | tr -d ' \n' > /tmp/su.status
grep -q 'last_applied:Some(SystemUpdateResult{at:[0-9]*,serial:9,snapshot:"test-f"' /tmp/su.status &&
	grep -q 'automatic:true,}' /tmp/su.status
r=$?; [ "$r" = 0 ] || head -c 1500 /tmp/su.status
result "the status records the update as automatic" "$r"
for _ in $(seq 1 20); do [ "$(cat /run/nodo-snaptest.version 2>/dev/null)" = 1.1 ] && break; sleep 1; done
pid_after=$(sv_pid)
if [ "$(cat /run/nodo-snaptest.version)" = 1.1 ] && [ -n "$pid_after" ] && [ "$pid_after" != "$pid_before" ]; then r=0; else r=1; fi
result "the service came back on the new version after the automatic update" "$r"
offer test-g
says "needs_restart: true" ctl system-check && says "waits for you: it needs a restart" ctl system-run &&
	unchanged nodo-snaptest-1.1_1 9
result "inside the window, a snapshot that needs a restart still waits for the owner" "$?"
start_schedule
sleep 15
sv force-stop nodo-systemupdate >/dev/null 2>&1; rm -f /var/service/nodo-systemupdate
unchanged nodo-snaptest-1.1_1 9
result "the schedule leaves it alone too" "$?"
ctl system-auto 0 >/dev/null
if [ -f /tmp/su.policy ]; then cp /tmp/su.policy "$policy"; else rm -f "$policy"; fi
exit "$fail"
