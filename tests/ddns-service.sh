#!/bin/sh
# Runs on the device under test with nodo-ddns, nodo-caddy and curl installed. Runs the packaged
# nodo-ddns service against a fake Cloudflare API and fake IP echo services, served by Caddy in a
# network namespace on an internet-like address. /etc/hosts points the real hostnames there and a
# throwaway CA is trusted for the duration, so the real ureq client, TLS and main loop all run.
set -u
ns=ddnsinet
peer=198.51.100.1
fake=/tmp/fakeapi
ip_seen=93.184.216.34
zone=023e105f4ecef8ad9ca31a8372d0c353
token=cf-test-token-1234567890
conf=/run/nodo/secrets/nodo-ddns/config.toml
hosts="api.cloudflare.com api.ipify.org ipv4.icanhazip.com checkip.amazonaws.com v4.ident.me ipv4.wtfismyip.com"
fail=0
result() { if [ "$2" = 0 ]; then echo "PASS $1"; else echo "FAIL $1"; fail=1; fi; }

# Services stay linked between runs and are only switched down, with a down file so a reboot keeps
# them down. Removing a link makes runsv linger while its log runs, and a lingering one ignores the
# next start.
service_up() {
	rm -f "/etc/sv/$1/down"
	ln -sfn "/etc/sv/$1" "/etc/runit/runsvdir/default/$1"
	i=0; until sv -w 5 up "$1" >/dev/null 2>&1 || [ "$i" -ge 10 ]; do i=$((i+1)); sleep 2; done
}
service_down() {
	[ -d "/etc/sv/$1" ] || return 0
	touch "/etc/sv/$1/down"
	sv -w 15 down "$1" >/dev/null 2>&1
}
cleanup() {
	service_down nodo-ddns
	rm -f "$conf" /var/lib/nodo-ddns/state.json
	ip netns pids "$ns" 2>/dev/null | xargs -r kill 2>/dev/null
	ip netns del "$ns" 2>/dev/null
	ip link del ddns0 2>/dev/null
	sed -i '/# nodo-ddns-test$/d' /etc/hosts
	rm -f /usr/local/share/ca-certificates/nodo-ddns-test.crt
	rmdir -p /usr/local/share/ca-certificates 2>/dev/null
	update-ca-certificates >/dev/null 2>&1
	rm -rf "$fake"
}
trap cleanup EXIT

ip netns add "$ns"
ip link add ddns0 type veth peer name eth0 netns "$ns"
ip addr add 198.51.100.2/24 dev ddns0
ip link set ddns0 up
ip netns exec "$ns" ip addr add "$peer/24" dev eth0
ip netns exec "$ns" ip link set lo up
ip netns exec "$ns" ip link set eth0 up

# The fake API answers only with the expected bearer token, and logs every request body.
mkdir -p "$fake"
cat > "$fake/config.json" <<JSON
{"admin":{"disabled":true},
 "logging":{"logs":{"default":{"exclude":["http.log.access"]},
  "access":{"writer":{"output":"file","filename":"$fake/access.log"},"encoder":{"format":"json"},"include":["http.log.access"]}}},
 "apps":{
  "tls":{"automation":{"policies":[{"issuers":[{"module":"internal"}]}]}},
  "pki":{"certificate_authorities":{"local":{"install_trust":false}}},
  "http":{"servers":{"fake":{"listen":["$peer:443"],"logs":{},"automatic_https":{"disable_redirects":true},
   "routes":[
    {"match":[{"host":["api.cloudflare.com"]}],"handle":[{"handler":"subroute","routes":[
     {"handle":[{"handler":"log_append","key":"body","value":"{http.request.body}"}]},
     {"match":[{"not":[{"header":{"Authorization":["Bearer $token"]}}]}],
      "handle":[{"handler":"static_response","status_code":403,"body":"{\"success\":false,\"errors\":[{\"code\":10000,\"message\":\"Authentication error\"}]}"}]},
     {"match":[{"method":["GET"],"path":["/client/v4/zones"],"query":{"name":["example.com"]}}],
      "handle":[{"handler":"static_response","body":"{\"success\":true,\"errors\":[],\"result\":[{\"id\":\"$zone\",\"name\":\"example.com\"}]}"}]},
     {"match":[{"method":["GET"],"path":["/client/v4/zones/$zone/dns_records"],"query":{"name.exact":["shop.example.com"]}}],
      "handle":[{"handler":"static_response","body":"{\"success\":true,\"errors\":[],\"result\":[{\"id\":\"rec-shop\",\"name\":\"shop.example.com\",\"type\":\"A\",\"content\":\"198.51.100.1\"}]}"}]},
     {"match":[{"method":["GET"],"path":["/client/v4/zones/$zone/dns_records"]}],
      "handle":[{"handler":"static_response","body":"{\"success\":true,\"errors\":[],\"result\":[]}"}]},
     {"match":[{"method":["POST","PATCH","DELETE"],"path":["/client/v4/zones/$zone/dns_records","/client/v4/zones/$zone/dns_records/*"]}],
      "handle":[{"handler":"static_response","body":"{\"success\":true,\"errors\":[],\"result\":{\"id\":\"written\"}}"}]},
     {"handle":[{"handler":"static_response","status_code":404}]}]}]},
    {"match":[{"host":["api.ipify.org","ipv4.icanhazip.com","checkip.amazonaws.com","v4.ident.me","ipv4.wtfismyip.com"]}],
     "handle":[{"handler":"static_response","body":"$ip_seen\n"}]}]}}}}}
JSON
XDG_DATA_HOME=$fake XDG_CONFIG_HOME=$fake HOME=$fake \
	nohup ip netns exec "$ns" nodo-caddy run --config "$fake/config.json" >"$fake/caddy.log" 2>&1 </dev/null &
i=0; until [ -s "$fake/caddy/pki/authorities/local/root.crt" ] || [ "$i" -ge 20 ]; do i=$((i+1)); sleep 1; done
sleep 2
mkdir -p /usr/local/share/ca-certificates
cp "$fake/caddy/pki/authorities/local/root.crt" /usr/local/share/ca-certificates/nodo-ddns-test.crt
update-ca-certificates >/dev/null 2>&1
# Tagged lines, so an interrupted earlier run cannot leave entries that shadow these.
sed -i '/# nodo-ddns-test$/d' /etc/hosts
for h in $hosts; do echo "$peer $h # nodo-ddns-test" >> /etc/hosts; done
code=$(curl -s -o /dev/null -w '%{http_code}' --max-time 5 https://api.ipify.org/)
r=1; [ "$code" = 200 ] && r=0
result "fake internet answers over trusted TLS ($code)" "$r"

write_conf() {
	install -d -m 711 /run/nodo/secrets
	install -d -m 700 -o _nododdns -g _nododdns /run/nodo/secrets/nodo-ddns
	install -m 600 -o _nododdns -g _nododdns /dev/null "$conf"
	cat > "$conf" <<TOML
domain = "example.com"
state_file = "/var/lib/nodo-ddns/state.json"
records = [$1]
ttl = 300
interval_secs = 60

[dns]
provider = "cloudflare"
api_token = "$token"
TOML
}
# Only log lines written after the last mark count, so lines from earlier runs cannot satisfy a check.
mark() { mark=$(wc -l < /var/log/nodo-ddns/current 2>/dev/null || echo 0); }
new_log() { tail -n "+$((mark + 1))" /var/log/nodo-ddns/current 2>/dev/null; }
wait_log() { i=0; until new_log | grep -q "$1" || [ "$i" -ge 40 ]; do i=$((i+1)); sleep 1; done; new_log | grep -q "$1"; }

mark
service_up nodo-ddns
wait_log "does not exist"
result "service waits without a config" "$?"

write_conf '"shop"'
mark
i=0; until sv -w 5 restart nodo-ddns >/dev/null 2>&1 || [ "$i" -ge 6 ]; do i=$((i+1)); sleep 2; done
wait_log '"kind":"updated"'
result "first pass updates the record" "$?"
p=$(pgrep -u _nododdns -x nodo-ddns | head -1)
if [ -n "$p" ] && [ ! -e "/proc/$p/root/etc/shadow" ] && [ "$(readlink "/proc/$p/ns/pid")" != "$(readlink /proc/1/ns/pid)" ]; then r=0; else r=1; fi
result "nodo-ddns runs sandboxed as _nododdns" "$r"
grep '"method":"PATCH"' "$fake/access.log" | grep -q 'dns_records/rec-shop'
result "existing record is patched by id" "$?"
grep '"method":"PATCH"' "$fake/access.log" | grep -q "\\\\\"content\\\\\":\\\\\"$ip_seen\\\\\""
result "patch carries the agreed address" "$?"
for h in api.ipify.org ipv4.icanhazip.com checkip.amazonaws.com v4.ident.me ipv4.wtfismyip.com; do
	grep -q "\"host\":\"$h\"" "$fake/access.log" || { echo "not asked: $h"; r=1; }
done
result "every IPv4 echo service was asked" "${r:-0}"
! grep -q '"status":403' "$fake/access.log"
result "every API call carried the token" "$?"
grep -q "\"shop/A\"" /var/lib/nodo-ddns/state.json && grep -q "$ip_seen" /var/lib/nodo-ddns/state.json &&
	[ "$(stat -c '%U %a' /var/lib/nodo-ddns/state.json)" = "_nododdns 600" ]
result "state records the address, private to the service" "$?"
! new_log | grep -q -e "$ip_seen" -e "$token"
result "log holds neither the home IP nor the token" "$?"

# The site is renamed: the old record becomes stale and is only reported.
write_conf '"blog"'
: > "$fake/access.log"
mark
sv -w 30 restart nodo-ddns >/dev/null
wait_log '"stale":\["shop/A"\]'
result "a pass reports the stale record" "$?"
grep '"method":"POST"' "$fake/access.log" | grep -q 'blog.example.com'
result "a new record is created" "$?"
! grep -q '"method":"DELETE"' "$fake/access.log"
result "stale records are not deleted by a pass" "$?"
out=$(chpst -u _nododdns:_nododdns nodo-ddns --config "$conf" --list-stale 2>&1)
r=1; [ "$out" = '{"stale":["shop/A"]}' ] && r=0
result "--list-stale names it without contacting anyone ($out)" "$r"
! grep -q '"method":"DELETE"' "$fake/access.log"
result "listing deleted nothing" "$?"
sv -w 30 stop nodo-ddns >/dev/null
out=$(chpst -u _nododdns:_nododdns nodo-ddns --config "$conf" --retire-stale 2>&1)
[ "$out" = '{"removed":["shop/A"]}' ] && grep '"method":"DELETE"' "$fake/access.log" | grep -q 'dns_records/rec-shop'
result "--retire-stale deletes it when asked ($out)" "$?"
! grep -q '"shop/A"' /var/lib/nodo-ddns/state.json
result "retired record leaves the state" "$?"

# Credentials that the provider rejects back off instead of hammering the API.
sed -i "s/$token/wrong-token/" "$conf"
rm -f /var/lib/nodo-ddns/state.json
mark
sv -w 30 start nodo-ddns >/dev/null
wait_log '"kind":"failed"'
result "a rejected token is reported as failed" "$?"
if new_log | grep -q "wrong-token"; then r=1; else r=0; fi
result "the rejected token is not logged" "$r"

sv -w 30 stop nodo-ddns | grep -q '^ok: down'
result "nodo-ddns stops cleanly under TERM" "$?"
exit "$fail"
