#!/bin/sh
# Runs on the device under test with nodo-caddy and curl installed, and the generated fixture in /tmp/web.
# Loads the config nodo-web generates into the real packaged Caddy and sends real requests from two
# network namespaces: one on a LAN address and one on an internet address. The LAN client sending to
# the forwarded port is exactly what a router that masquerades port forwards delivers.
#
# The VM is offline, so public hosts use Caddy's internal CA instead of ACME. See the fixture test in
# crates/nodo-web/src/caddy/tests.rs.
set -u
fix=/tmp/web
onion=abcdefghijklmnopqrstuvwxyz234567abcdefghijklmnopqrstuvwx.onion
lan=192.168.77.2
wan=203.0.113.2
root_crt=/var/lib/caddy/data/caddy/pki/authorities/local/root.crt
fail=0
result() { if [ "$2" = 0 ]; then echo "PASS $1"; else echo "FAIL $1"; fail=1; fi; }
expect() { name=$1; want=$2; got=$3; if [ "$got" = "$want" ]; then echo "PASS $name"; else echo "FAIL $name: want $want, got $got"; fail=1; fi; }

cleanup() {
	pkill -u nobody -x nodo-caddy 2>/dev/null
	for ns in weblan webinet; do
		ip netns pids "$ns" 2>/dev/null | xargs -r kill 2>/dev/null
		ip netns del "$ns" 2>/dev/null
	done
	ip link del weblan0 2>/dev/null
	ip link del webinet0 2>/dev/null
	rm -f /etc/runit/runsvdir/default/nodo-caddy /etc/nftables.d/nodo-web.nft /var/lib/nodo-web/caddy.json
	nft -f /etc/nftables.conf
	rm -rf /tmp/up-shop /tmp/up-blog /tmp/body
}
trap cleanup EXIT

# Prints the HTTP status, or ERR<curl exit code> when no HTTP response came back. The body goes to /tmp/body.
get() {
	ns=$1; shift
	: > /tmp/body
	if [ "$ns" = host ]; then set -- curl "$@"; else set -- ip netns exec "$ns" curl "$@"; fi
	code=$("$@" -s -o /tmp/body -w '%{http_code}' --cacert "$root_crt" --connect-timeout 3 --max-time 6 2>/dev/null)
	rc=$?
	if [ "$rc" != 0 ]; then echo "ERR$rc"; else echo "$code"; fi
}
body_has() { grep -q "$1" /tmp/body; }

nodo-caddy validate --config "$fix/caddy.json" >/dev/null 2>&1
result "caddy validate accepts the generated config" "$?"
printf '{"apps":{"http":{"servers":{"x":{"listen":[":1"],"routes":[{"handle":[{"handler":"nope"}]}]}}}}}' > /tmp/bad.json
! nodo-caddy validate --config /tmp/bad.json >/dev/null 2>&1
result "caddy validate rejects an unknown handler" "$?"
rm -f /tmp/bad.json

# Fake upstreams answer with their own name and the path they received, so leaks are visible.
upstream() {
	mkdir -p "/tmp/up-$1"
	cat > "/tmp/up-$1/config.json" <<JSON
{"admin":{"disabled":true},"apps":{"http":{"servers":{"u":{"listen":["127.0.0.1:$2"],
 "automatic_https":{"disable":true},
 "routes":[{"handle":[{"handler":"static_response","body":"upstream $1 {http.request.uri}"}]}]}}}}}
JSON
	chown -R nobody "/tmp/up-$1"
	HOME=/tmp/up-$1 XDG_DATA_HOME=/tmp/up-$1 XDG_CONFIG_HOME=/tmp/up-$1 \
		nohup chpst -u nobody nodo-caddy run --config "/tmp/up-$1/config.json" >"/tmp/up-$1/log" 2>&1 </dev/null &
}
upstream shop 3001
upstream blog 3004

ip netns add weblan
ip netns add webinet
ip link add weblan0 type veth peer name eth0 netns weblan
ip link add webinet0 type veth peer name eth0 netns webinet
ip addr add "$lan/24" dev weblan0
ip addr add "$wan/24" dev webinet0
ip link set weblan0 up
ip link set webinet0 up
ip netns exec weblan ip addr add 192.168.77.1/24 dev eth0
ip netns exec webinet ip addr add 203.0.113.1/24 dev eth0
for ns in weblan webinet; do ip netns exec "$ns" ip link set lo up; ip netns exec "$ns" ip link set eth0 up; done

install -m 640 -g _caddy "$fix/caddy.json" /var/lib/nodo-web/caddy.json
install -m 644 "$fix/web.nft" /etc/nftables.d/nodo-web.nft
nft -f /etc/nftables.conf
result "generated nftables rules load" "$?"
ln -sfn /etc/sv/nodo-caddy /etc/runit/runsvdir/default/nodo-caddy
# runsvdir rescans every few seconds. A supervisor left from an earlier run remembers being stopped.
i=0; until sv -w 5 start nodo-caddy >/dev/null 2>&1 || [ "$i" -ge 6 ]; do i=$((i+1)); sleep 2; done
i=0; until { [ -s "$root_crt" ] && grep -q ':20FB ' /proc/net/tcp6 /proc/net/tcp; } || [ "$i" -ge 30 ]; do i=$((i+1)); sleep 1; done
sv status nodo-caddy | grep -q '^run:'
result "nodo-caddy service runs" "$?"
# Certificates are issued in the background after the listener opens.
sleep 3

p=$(pgrep -u _caddy -x nodo-caddy | head -1)
if [ -n "$p" ] && [ "$(stat -c %U "/proc/$p")" = _caddy ] && [ ! -e "/proc/$p/root/etc/shadow" ] &&
	[ "$(readlink "/proc/$p/ns/pid")" != "$(readlink /proc/1/ns/pid)" ]; then r=0; else r=1; fi
result "caddy runs sandboxed as _caddy" "$r"

pub() { get webinet --resolve "$1:443:$wan" "https://$1$2" ${3:-}; }

# Public hosts from an internet client, through the 443 redirect.
expect "public host reaches its upstream" 200 "$(pub shop.example.com /)"
body_has "upstream shop /"; result "public host is served by the shop upstream" "$?"
for path in /admin /ADMIN /Admin/users /admin/ /admin/x //admin ///admin /admin//x '/admin;x=1' /admin%2Fx \
	/%61dmin /%41DMIN /./admin /x/../admin /x/%2e%2e/admin '/admin?x=1' /admin%00 /admin. /admin%20 \
	//ADMIN. /x/../admin%00 /admin%09 /admin.json; do
	code=$(pub shop.example.com "$path" --path-as-is)
	if body_has upstream; then leaked=1; else leaked=0; fi
	case $code in 404|400) ok=$leaked ;; *) ok=1 ;; esac
	result "public admin path blocked: $path ($code)" "$ok"
done
for path in /administrator /admin-panel /admins; do
	expect "neighbouring path is served: $path" 200 "$(pub shop.example.com "$path")"
done
expect "locked site shows the unavailable page" 503 "$(pub vault.example.com /)"
body_has "This site is unavailable"; result "unavailable page body" "$?"
expect "stopped site shows the unavailable page" 503 "$(pub pay.example.com /)"
# Admin paths answer the same whatever the upstream state, so they reveal nothing about it.
expect "locked site still hides admin paths" 404 "$(pub vault.example.com /admin)"

# Host isolation on the public listener.
expect "local host refused on the public port" ERR35 "$(pub shop.local /)"
expect "unknown host refused on the public port" ERR35 "$(pub other.example.com /)"
code=$(get webinet --resolve "shop.example.com:443:$wan" -H "Host: shop.local" https://shop.example.com/admin)
! body_has upstream && [ "$code" != 200 ]
result "SNI of a public host cannot reach a local host by Host header ($code)" "$?"
# curl refuses to resolve .onion names, so the onion host is only sent in the Host header.
code=$(get webinet --resolve "shop.example.com:443:$wan" -H "Host: $onion" https://shop.example.com/)
! body_has upstream && [ "$code" != 200 ]
result "onion host refused on the public port ($code)" "$?"
code=$(get webinet --resolve "shop.example.com:443:$wan" -H "Host: blog.example.com" https://shop.example.com/)
! body_has upstream && [ "$code" != 200 ]
result "SNI of one public host cannot reach another ($code)" "$?"

# Ports that must stay closed to the internet.
expect "local port closed to the internet" ERR28 "$(get webinet --resolve "shop.local:8443:$wan" https://shop.local:8443/)"
expect "caddy's own public port is not reachable directly" ERR28 \
	"$(get webinet --resolve "shop.example.com:10443:$wan" https://shop.example.com:10443/)"
code=$(get webinet -k "https://$wan:19000/")
case $code in ERR7|ERR28) r=0 ;; *) r=1 ;; esac
result "onion listener not reachable from the network ($code)" "$r"

# Plain HTTP only redirects. Caddy redirects any Host to https on the same name, which echoes what the
# client sent and serves nothing, since that name is then refused on 443.
code=$(get webinet -D /tmp/headers --resolve "shop.example.com:80:$wan" http://shop.example.com/x)
grep -qi '^location: https://shop.example.com/x' /tmp/headers && [ "$code" = 308 ]
result "port 80 redirects public hosts to https without a port ($code)" "$?"
code=$(get webinet -D /tmp/headers --resolve "shop.local:80:$wan" http://shop.local/admin)
! body_has upstream && { [ "$code" = 308 ] || [ "${code#ERR}" != "$code" ]; }
result "port 80 serves no content for a local host ($code)" "$?"
rm -f /tmp/headers

# Local hosts from a LAN client.
loc() { get weblan --resolve "$1:8443:$lan" "https://$1:8443$2" ${3:-}; }
expect "local host reaches its upstream" 200 "$(loc shop.local /)"
expect "local host serves admin paths to the LAN" 200 "$(loc shop.local /admin)"
body_has "upstream shop /admin"; result "local admin path reaches the shop upstream" "$?"
expect "stopped local site shows the unavailable page" 503 "$(loc pay.local /)"
expect "public host refused on the local port" ERR35 "$(loc shop.example.com /)"
code=$(get weblan --resolve "shop.local:8443:$lan" -H "Host: $onion" https://shop.local:8443/)
! body_has upstream && [ "$code" != 200 ]
result "onion host refused on the local port ($code)" "$?"

# What a masquerading router delivers: internet traffic on the forwarded port, from a LAN address.
expect "masqueraded client cannot reach a local host" ERR35 \
	"$(get weblan --resolve "shop.local:443:$lan" https://shop.local/)"
code=$(get weblan --resolve "shop.example.com:443:$lan" https://shop.example.com/admin)
! body_has upstream && [ "$code" = 404 ]
result "masqueraded client cannot reach admin paths ($code)" "$?"
code=$(get weblan --resolve "shop.example.com:443:$lan" -H "Host: shop.local" https://shop.example.com/admin)
! body_has upstream
result "masqueraded client cannot reach a local host by Host header ($code)" "$?"

# The onion listener, as Tor reaches it.
expect "onion host reaches its upstream" 200 "$(get host -H "Host: $onion" http://127.0.0.1:19000/)"
expect "onion admin path blocked" 404 "$(get host -H "Host: $onion" http://127.0.0.1:19000//ADMIN)"
expect "onion listener refuses other hosts" ERR52 "$(get host -H 'Host: shop.example.com' http://127.0.0.1:19000/)"
expect "onion listener refuses local hosts" ERR52 "$(get host -H 'Host: shop.local' http://127.0.0.1:19000/admin)"

# Rate limiting is per client address, on public routes that asked for it. The fixture allows 5 a minute.
codes=
for _ in 1 2 3 4 5 6; do codes="$codes $(pub blog.example.com /)"; done
expect "rate limit allows 5 then refuses" " 200 200 200 200 200 429" "$codes"
expect "rate limit keeps a separate budget per client" 200 \
	"$(get weblan --resolve "blog.example.com:443:$lan" https://blog.example.com/)"
codes=
for _ in 1 2 3 4 5 6 7; do codes="$codes $(pub shop.example.com /)"; done
case $codes in *429*) r=1 ;; *) r=0 ;; esac
result "sites that did not ask are not rate limited" "$r"

# An upstream that dies after the config was generated.
pkill -u nobody -f 'up-shop/config.json'
sleep 1
expect "dead upstream shows the unavailable page" 502 "$(pub shop.example.com /)"
body_has "This site is unavailable"; result "dead upstream page body" "$?"
expect "dead upstream still hides admin paths" 404 "$(pub shop.example.com /admin)"

sv -w 30 stop nodo-caddy | grep -q '^ok: down'
result "nodo-caddy stops cleanly under TERM" "$?"
grep -q 'shutting down' /var/log/nodo-caddy/current
result "caddy logged a graceful shutdown" "$?"
exit "$fail"
