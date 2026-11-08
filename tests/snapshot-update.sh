#!/bin/sh
# Runs on the device under test, as root, after tests/snapshot-prepare.sh put test snapshots in
# /srv/snapshot-test. Needs nodo-tools (nodo-sysupdate) and nodo-caddy. A fake update server, Caddy
# in a network namespace on an internet-like address, serves them over HTTPS with a throwaway CA
# and over plain HTTP, and the real client moves the system between them:
#
#   it applies a signed snapshot over HTTP, refuses an unsigned, tampered, foreign or expired one,
#   a tampered index and a tampered package, applies the next one over HTTPS so that a service
#   updates and comes back, and then refuses the older snapshot and a conflicting one.
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
install -D -m 644 "$src/pins.toml" "$pins"
rm -f "$state"
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
[ "$(cat /run/nodo-snaptest.version)" = 1.1 ] && [ -n "$pid_after" ] && [ "$pid_after" != "$pid_before" ]
result "the service came back running the new version" "$?"
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
exit "$fail"
