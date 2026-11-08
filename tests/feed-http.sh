#!/bin/sh
# Runs on the device under test, as root, with nodo-tools and nodo-caddy installed and python3 from
# the mirror (tests/acme-prepare.sh pushes it). Feeds are served by Caddy in a network namespace on
# an internet-like address, over HTTPS with a throwaway CA trusted for the duration and over plain
# HTTP, and the real helper adds, refreshes and installs from them:
#
#   feeds.example.test     the honest feed, over HTTPS and plain HTTP
#   redirect.example.test  redirects into it, down to plain HTTP, and in a loop
#   github.example.test    a GitHub releases layout: latest/download redirects to a tag, which
#                          redirects to a storage host, objects.example.test
#   slow.example.test:81   accepts connections and never answers
#
# An attacker on the network is an nftables rule that sends this device's connections to the
# feed host to a second Caddy, with its own CA, serving what the attacker chooses.
#   scripts/dev-vm.sh ssh 'cat > /tmp/feed-http.sh' < tests/feed-http.sh
#   scripts/dev-vm.sh ssh 'sh /tmp/feed-http.sh </dev/null'
set -u
ns=feedinet
peer=192.0.2.1
work=/srv/feed-http
www=$work/www
keys=$work/keys
hosts="feeds.example.test redirect.example.test github.example.test objects.example.test slow.example.test"
fail=0
result() { if [ "$2" = 0 ]; then echo "PASS $1"; else echo "FAIL $1"; fail=1; fi; }
ctl() { nodo-tui --ctl "$@"; }
# Succeeds when the command's output contains the text. The output is kept for a failure.
says() { text=$1; shift; "$@" > /tmp/fh.out 2>&1; grep -q -- "$text" /tmp/fh.out || { sed 's/^/    /' /tmp/fh.out | head -n 15; return 1; }; }
field() { sed -n "s/.*$1: \"\{0,1\}\([^\",]*\)\"\{0,1\},\{0,1\}$/\1/p" | head -n 1; }

mitm_off() { nft delete table ip feedmitm 2>/dev/null; return 0; }
# Sends this device's connections to the feed host's port $1 to the attacker's port $2.
mitm_on() {
	mitm_off
	nft add table ip feedmitm
	nft add chain ip feedmitm out '{ type nat hook output priority -100; }'
	nft add rule ip feedmitm out ip daddr "$peer" tcp dport "$1" dnat to "$peer:$2"
}
cleanup() {
	mitm_off
	for id in fh-app fh-plain fh-gh; do
		[ -d "/etc/sv/$id" ] && { touch "/etc/sv/$id/down"; sv -w 15 down "$id" >/dev/null 2>&1; }
	done
	for f in $(ctl feeds 2>/dev/null | sed -n 's/.*name: "\(fh-[a-z0-9-]*\)".*/\1/p'); do ctl remove-feed "$f" >/dev/null 2>&1; done
	ip netns pids "$ns" 2>/dev/null | xargs -r kill 2>/dev/null
	ip netns del "$ns" 2>/dev/null
	ip link del feed0 2>/dev/null
	sed -i '/# nodo-feed-test$/d' /etc/hosts
	rm -f /usr/local/share/ca-certificates/nodo-feed-test.crt
	update-ca-certificates >/dev/null 2>&1
}
trap cleanup EXIT
command -v python3 >/dev/null || { echo "FAIL python3 is needed: scripts/dev-vm.sh mirror python3"; exit 1; }
[ -L /var/service/nodo-helper ] || ln -s /etc/sv/nodo-helper /var/service/
for _ in $(seq 1 30); do ctl services >/dev/null 2>&1 && break; sleep 1; done

rm -rf "$work"
mkdir -p "$www/honest/feed" "$www/gh/assets/v2" "$www/mitm/feed" "$work/pkgs" "$keys" "$work/caddy-honest" "$work/caddy-mitm"
for k in honest rotated attacker; do openssl genrsa -out "$keys/$k.pem" 2048 2>/dev/null; done

# Packages: each app only sleeps and says which version it is.
mkpkg() {
	name=$1 ver=$2 note=${3:-}
	d=$work/pkgs/$name-$ver
	rm -rf "$d"
	mkdir -p "$d/opt/nodo/apps/$name/bin"
	printf 'api_version = 1\nid = "%s"\nversion = "%s"\nexec = ["bin/run"]\nstop_timeout_secs = 5\n' "$name" "$ver" > "$d/opt/nodo/apps/$name/manifest.toml"
	printf '#!/bin/sh\necho "%s %s %s"\nwhile :; do sleep 1; done\n' "$name" "$ver" "$note" > "$d/opt/nodo/apps/$name/bin/run"
	chmod -R u=rwX,go=rX "$d"
	chmod 755 "$d/opt/nodo/apps/$name/bin/run"
	(cd "$work/pkgs" && xbps-create -A x86_64 -n "$name-${ver}_1" -s "feed test app" "$d" >/dev/null)
	echo "$work/pkgs/$name-${ver}_1.x86_64.xbps"
}
# publish <dir> <key> <package>...: adds the packages to a feed directory and signs it.
publish() {
	dir=$1 key=$2; shift 2
	for p in "$@"; do
		cp "$p" "$dir/" && xbps-rindex -f -a "$dir/$(basename "$p")" >/dev/null || return 1
	done
	xbps-rindex --signedby feed-test --privkey "$key" -s "$dir" >/dev/null &&
		for p in "$@"; do xbps-rindex --privkey "$key" -S "$dir/$(basename "$p")" >/dev/null || return 1; done
}
app10=$(mkpkg fh-app 1.0); app11=$(mkpkg fh-app 1.1)
plain10=$(mkpkg fh-plain 1.0); plain11=$(mkpkg fh-plain 1.1)
gh10=$(mkpkg fh-gh 1.0)
evil11=$(mkpkg fh-plain 1.1 "but built by the attacker")
publish "$www/honest/feed" "$keys/honest.pem" "$app10" "$plain10"
publish "$www/gh/assets/v2" "$keys/honest.pem" "$gh10"

ip netns add "$ns"
ip link add feed0 type veth peer name eth0 netns "$ns"
ip addr add 192.0.2.2/24 dev feed0
ip link set feed0 up
ip netns exec "$ns" ip addr add "$peer/24" dev eth0
ip netns exec "$ns" ip link set lo up
ip netns exec "$ns" ip link set eth0 up

files() { printf '{"handler":"file_server","root":"%s"}' "$1"; }
found() { printf '{"handler":"static_response","status_code":302,"headers":{"Location":["%s"]}}' "$1"; }
cat > "$work/caddy-honest/config.json" <<JSON
{"admin":{"disabled":true},"logging":{"logs":{"default":{"level":"ERROR"}}},
 "apps":{
  "tls":{"automation":{"policies":[{"issuers":[{"module":"internal"}]}]}},
  "pki":{"certificate_authorities":{"local":{"install_trust":false}}},
  "http":{"servers":{
   "tls":{"listen":["$peer:443"],"automatic_https":{"disable_redirects":true},"routes":[
    {"match":[{"host":["feeds.example.test"]}],"handle":[$(files "$www/honest")]},
    {"match":[{"host":["redirect.example.test"],"path":["/feed/*"]}],"handle":[$(found "https://feeds.example.test{http.request.uri.path}")]},
    {"match":[{"host":["redirect.example.test"],"path":["/down/*"]}],"handle":[$(found "http://feeds.example.test/feed/{http.request.uri.path.file}")]},
    {"match":[{"host":["redirect.example.test"],"path":["/loop/*"]}],"handle":[$(found "/loop/{http.request.uri.path.file}")]},
    {"match":[{"host":["github.example.test"],"path":["/owner/repo/releases/latest/download/*"]}],
     "handle":[$(found "/owner/repo/releases/download/v2/{http.request.uri.path.file}")]},
    {"match":[{"host":["github.example.test"],"path":["/owner/repo/releases/download/*"]}],
     "handle":[$(found "https://objects.example.test/assets/{http.request.uri.path.4}/{http.request.uri.path.5}?X-Amz-Signature=test")]},
    {"match":[{"host":["objects.example.test"]}],"handle":[$(files "$www/gh")]}]},
   "plain":{"listen":["$peer:80"],"automatic_https":{"disable":true},"routes":[
    {"match":[{"host":["feeds.example.test"]}],"handle":[$(files "$www/honest")]}]}}}}}
JSON
# The attacker's Caddy has its own CA, which this device does not trust.
cat > "$work/caddy-mitm/config.json" <<JSON
{"admin":{"disabled":true},"logging":{"logs":{"default":{"level":"ERROR"}}},
 "apps":{
  "tls":{"automation":{"policies":[{"issuers":[{"module":"internal"}]}]}},
  "pki":{"certificate_authorities":{"local":{"install_trust":false}}},
  "http":{"servers":{
   "tls":{"listen":["$peer:8443"],"automatic_https":{"disable_redirects":true},"routes":[
    {"match":[{"host":["feeds.example.test"]}],"handle":[$(files "$www/mitm")]}]},
   "plain":{"listen":["$peer:8080"],"automatic_https":{"disable":true},"routes":[
    {"match":[{"host":["feeds.example.test"]}],"handle":[$(files "$www/mitm")]}]}}}}}
JSON
for c in honest mitm; do
	d=$work/caddy-$c
	XDG_DATA_HOME=$d XDG_CONFIG_HOME=$d HOME=$d \
		nohup ip netns exec "$ns" nodo-caddy run --config "$d/config.json" >"$d/caddy.log" 2>&1 </dev/null &
done
# A server that accepts connections and never answers.
nohup ip netns exec "$ns" python3 -c '
import socket, time
s = socket.socket(); s.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
s.bind(("'"$peer"'", 81)); s.listen(16); held = []
while True:
    held.append(s.accept()[0])' >/dev/null 2>&1 </dev/null &
i=0; until [ -s "$work/caddy-honest/caddy/pki/authorities/local/root.crt" ] || [ "$i" -ge 20 ]; do i=$((i+1)); sleep 1; done
mkdir -p /usr/local/share/ca-certificates
cp "$work/caddy-honest/caddy/pki/authorities/local/root.crt" /usr/local/share/ca-certificates/nodo-feed-test.crt
update-ca-certificates >/dev/null 2>&1
sed -i '/# nodo-feed-test$/d' /etc/hosts
for h in $hosts; do echo "$peer $h # nodo-feed-test" >> /etc/hosts; done
sleep 2

# Signed index over HTTPS and plain HTTP

probe_fp() { ctl probe-feed "$1" > /tmp/fh.probe 2>&1; field fingerprint < /tmp/fh.probe; }
fp=$(probe_fp https://feeds.example.test/feed)
[ -n "$fp" ] && grep -q "packages: 2" /tmp/fh.probe
result "an HTTPS feed is probed through the helper and shows its signing key" "$?"
[ "$(probe_fp http://feeds.example.test/feed)" = "$fp" ]
result "the same feed over plain HTTP shows the same key" "$?"
ctl add-feed https://feeds.example.test/feed "$fp" > /tmp/fh.out 2>&1 && ctl add-feed http://feeds.example.test/feed "$fp" >> /tmp/fh.out 2>&1
r=$?; [ "$r" = 0 ] || cat /tmp/fh.out
result "both are added with the confirmed key" "$r"
https_feed=$(ctl feeds | grep -B1 'address: "https://feeds.example.test/feed"' | field name)
http_feed=$(ctl feeds | grep -B1 'address: "http://feeds.example.test/feed"' | field name)
[ -n "$https_feed" ] && [ -n "$http_feed" ] || ctl feeds
says "problem: None" ctl approve-install "$https_feed" fh-app && [ "$(xbps-query -p pkgver fh-app)" = fh-app-1.0_1 ]
result "a package installs from the HTTPS feed" "$?"
says "problem: None" ctl approve-install "$http_feed" fh-plain && [ "$(xbps-query -p pkgver fh-plain)" = fh-plain-1.0_1 ]
result "a package installs from the plain HTTP feed, checked by its signature" "$?"

# A tampered package or index on the server

publish "$www/honest/feed" "$keys/honest.pem" "$app11" "$plain11"
cp "$www/honest/feed/fh-app-1.1_1.x86_64.xbps" "$work/app11.good"
# Same size, one byte different, so only the hash can tell.
printf 'X' | dd of="$www/honest/feed/fh-app-1.1_1.x86_64.xbps" bs=1 seek=200 conv=notrunc 2>/dev/null
ctl refresh-feeds >/dev/null
says "SHA-256 does not match" ctl review "$https_feed" fh-app && [ "$(xbps-query -p pkgver fh-app)" = fh-app-1.0_1 ]
result "a package changed on the server is refused by its hash and nothing installs" "$?"
cp "$work/app11.good" "$www/honest/feed/fh-app-1.1_1.x86_64.xbps"
cp "$www/honest/feed/x86_64-repodata" "$work/repodata.good"
printf 'garbage' | dd of="$www/honest/feed/x86_64-repodata" bs=1 seek=40 conv=notrunc 2>/dev/null
says "index" ctl probe-feed https://feeds.example.test/feed
result "a damaged index is reported plainly" "$?"
cp "$work/repodata.good" "$www/honest/feed/x86_64-repodata"

# An attacker on the network, against plain HTTP: signatures catch every change

mkmitm() { rm -rf "$www/mitm/feed"; mkdir -p "$www/mitm/feed"; cp "$www/honest/feed"/* "$www/mitm/feed/"; }
mkmitm
printf 'X' | dd of="$www/mitm/feed/fh-plain-1.1_1.x86_64.xbps" bs=1 seek=200 conv=notrunc 2>/dev/null
mitm_on 80 8080
says "SHA-256 does not match" ctl review "$http_feed" fh-plain && [ "$(xbps-query -p pkgver fh-plain)" = fh-plain-1.0_1 ]
result "MITM over HTTP: a swapped package is refused by its hash" "$?"

# The attacker's own package, indexed under the honest key's metadata, with the attacker's signature.
mkmitm
rm -f "$www/mitm/feed/fh-plain-1.1_1.x86_64.xbps"*
mkdir -p "$work/evil" "$work/forge"
cp "$evil11" "$work/evil/fh-plain-1.1_1.x86_64.xbps"
publish "$work/evil" "$keys/attacker.pem" "$work/evil/fh-plain-1.1_1.x86_64.xbps"
cp "$work/evil/fh-plain-1.1_1.x86_64.xbps" "$work/evil/fh-plain-1.1_1.x86_64.xbps.sig2" "$www/mitm/feed/"
rm -rf "$work/forge"/*
(cd "$work/forge" && zstd -dqc "$www/honest/feed/x86_64-repodata" | tar -xf - index-meta.plist &&
	zstd -dqc "$work/evil/x86_64-repodata" | tar -xf - index.plist &&
	tar -cf - index.plist index-meta.plist | zstd -q > "$www/mitm/feed/x86_64-repodata")
says "signature does not match" ctl review "$http_feed" fh-plain && [ "$(xbps-query -p pkgver fh-plain)" = fh-plain-1.0_1 ]
result "MITM over HTTP: a forged index and package fail the pinned key's signature" "$?"

# The attacker signs everything with their own key: that is a key change, and blocks the feed.
rm -rf "$www/mitm/feed"; mkdir -p "$www/mitm/feed"
publish "$www/mitm/feed" "$keys/attacker.pem" "$evil11"
says "key_changed: true" ctl refresh-feeds && says "key_changed: true" ctl review "$http_feed" fh-plain &&
	[ "$(xbps-query -p pkgver fh-plain)" = fh-plain-1.0_1 ]
result "MITM over HTTP: a feed re-signed by the attacker is blocked as a key change" "$?"

mitm_off
ctl refresh-feeds >/dev/null
says "Updated\|InstallReport\|problem: None" ctl apply-update fh-plain 1.1_1 ||
	says "problem: None" ctl approve-install "$http_feed" fh-plain
[ "$(xbps-query -p pkgver fh-plain)" = fh-plain-1.1_1 ]
result "with the attacker gone, the honest update installs over HTTP" "$?"
# Replaying the older, genuinely signed feed is a downgrade.
rm -rf "$www/mitm/feed"; mkdir -p "$www/mitm/feed"
publish "$www/mitm/feed" "$keys/honest.pem" "$plain10"
mitm_on 80 8080
says "1.0_1 is older than 1.1_1" ctl review "$http_feed" fh-plain && [ "$(xbps-query -p pkgver fh-plain)" = fh-plain-1.1_1 ]
result "MITM over HTTP: replaying an older signed version is refused as a downgrade" "$?"

# The same attacker against HTTPS cannot even complete the connection.
mitm_on 443 8443
says "certificate" ctl probe-feed https://feeds.example.test/feed
result "MITM over HTTPS: the attacker's certificate is refused" "$?"
ctl refresh-feeds > /dev/null
ctl feeds | grep -A8 "name: \"$https_feed\"" | grep -q "problem: Some(\".*certificate"
result "a refresh records the plain reason on the feed" "$?"
mitm_off
ctl refresh-feeds > /dev/null
! ctl feeds | grep -A8 "name: \"$https_feed\"" | grep -q "problem: Some"
result "the feed recovers once the attacker is gone" "$?"

# Redirects, including GitHub's release layout

[ "$(probe_fp https://redirect.example.test/feed)" = "$fp" ]
result "a redirect to the feed host is followed" "$?"
says "redirected from HTTPS to plain HTTP" ctl probe-feed https://redirect.example.test/down
result "a redirect from HTTPS down to plain HTTP is refused" "$?"
says "redirected more than 5 times" ctl probe-feed https://redirect.example.test/loop
result "a redirect loop stops" "$?"
gh=https://github.example.test/owner/repo/releases/latest/download
ghfp=$(probe_fp "$gh")
[ "$ghfp" = "$fp" ] && ctl add-feed "$gh" "$ghfp" >/dev/null
result "a GitHub releases feed is probed and added through two redirects to another host" "$?"
gh_feed=$(ctl feeds | grep -B1 "address: \"$gh\"" | field name)
says "problem: None" ctl approve-install "$gh_feed" fh-gh && [ "$(xbps-query -p pkgver fh-gh)" = fh-gh-1.0_1 ]
result "a package installs from the GitHub releases feed" "$?"

# Servers that are slow, closed, missing or absent

start=$(date +%s)
says "did not answer within 30 seconds" ctl probe-feed http://slow.example.test:81/feed
r=$?; took=$(($(date +%s) - start))
[ "$r" = 0 ] && [ "$took" -lt 45 ]
result "a server that never answers fails plainly after ${took}s" "$?"
says "refused the connection\|could not connect" ctl probe-feed http://feeds.example.test:8081/feed
result "a closed port is reported plainly" "$?"
start=$(date +%s)
says "could not be reached\|could not connect" ctl probe-feed http://192.0.2.99/feed
r=$?; took=$(($(date +%s) - start))
[ "$r" = 0 ] && [ "$took" -lt 45 ]
result "an address with no server is reported plainly after ${took}s" "$?"
says "has no x86_64-repodata (HTTP 404)" ctl probe-feed https://feeds.example.test/nothing
result "an address that is not a feed is reported plainly" "$?"

# An honest key rotation over HTTPS still needs the user

publish "$www/honest/feed" "$keys/rotated.pem" "$app11" "$plain11"
says "key_changed: true" ctl refresh-feeds && says "key_changed: true" ctl review "$https_feed" fh-app
result "a feed re-signed with a new key is blocked until the user decides" "$?"

for id in fh-app fh-plain fh-gh; do sv status "$id" | grep -q "^run:" || { sv status "$id"; r=1; }; done
result "every installed feed app runs" "${r:-0}"
exit "$fail"
