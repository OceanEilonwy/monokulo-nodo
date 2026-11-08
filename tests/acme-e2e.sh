# Sourced by tests/helper-e2e.sh after its web section, which leaves e2e-app installed and the
# device unlocked with PASS2. Proves certificates from a real ACME server: Pebble, Let's Encrypt's
# test CA, with pebble-challtestsrv as public DNS, both in the internet namespace. Nothing here can
# reach Let's Encrypt or a real DNS provider: the VM is offline, the helper is pointed at Pebble by
# /etc/nodo/acme-test.toml, and the "Cloudflare" API is tests/acme/fake-dns.py behind /etc/hosts.
#
# Caddy's caddy-dns/cloudflare module is the DNS-01 provider under test. Its API address is fixed,
# so /etc/hosts and a test CA send it to the fake without any test-only config, the same way
# tests/ddns-service.sh tests nodo-ddns. The fake copies records into challtestsrv.
#
# tests/acme-prepare.sh puts Pebble, pebble-challtestsrv, the fake and python3 into the VM.

acme=/tmp/acme
caddy_store=/var/lib/caddy/data/caddy
issuer_dir=$caddy_store/certificates/203.0.113.1-dir
tls_log=/var/lib/caddy/nodo-tls.log
CF_TOKEN=cfut_0123456789abcdefghijABCDEFGHIJ0123456789
ECHO_HOSTS="api.ipify.org ipv4.icanhazip.com checkip.amazonaws.com v4.ident.me ipv4.wtfismyip.com"

# Waits until `ctl certs` shows HOST in STATE, for up to SECS seconds.
cat > /tmp/e2e-bin/cert_state <<'EOF'
#!/bin/sh
host=$1; want=$2; secs=${3:-120}
for _ in $(seq 1 "$secs"); do
	nodo-tui --ctl certs > /tmp/e2e.certs 2>&1
	sed -n "/host: \"$host\"/,/last_error/p" /tmp/e2e.certs | grep -q "state: $want" && { cat /tmp/e2e.certs; exit 0; }
	sleep 1
done
cat /tmp/e2e.certs; tail -n 20 /var/lib/caddy/nodo-tls.log; exit 1
EOF
# One host's block of the last `ctl certs`.
cat > /tmp/e2e-bin/cert_block <<'EOF'
#!/bin/sh
nodo-tui --ctl certs | awk -v h="host: \"$1\"," 'index($0, h) { p = 1 } p && /SiteId \{|local_ca_fingerprint/ { exit } p' | sed 's/^ *//' | tr -d '\n'; echo
EOF
# SHA-256 fingerprint of the stored certificate, or of the one served, as NS IP HOST.
cat > /tmp/e2e-bin/stored_fp <<'EOF'
#!/bin/sh
openssl x509 -noout -fingerprint -sha256 -in "/var/lib/caddy/data/caddy/certificates/203.0.113.1-dir/$1/$1.crt" 2>/dev/null | cut -d= -f2
EOF
cat > /tmp/e2e-bin/served_fp <<'EOF'
#!/bin/sh
ns=$1; ip=$2; host=$3
set -- curl -s -o /dev/null --cacert /tmp/acme/pebble-roots.pem --connect-timeout 3 --max-time 6 \
	--resolve "$host:443:$ip" -w '%{certs}' "https://$host/"
[ "$ns" = e2einet ] && set -- "$@" --interface 203.0.113.10
ip netns exec "$ns" "$@" | openssl x509 -noout -fingerprint -sha256 2>/dev/null | cut -d= -f2
EOF
# Starts a fresh order for the public host: offline, stored certificate removed, public again.
cat > /tmp/e2e-bin/fresh_order <<'EOF'
#!/bin/sh
nodo-tui --ctl expose e2e-app shop "${1:-0}" 0 0 >/dev/null || exit 1
rm -rf "/var/lib/caddy/data/caddy/certificates/203.0.113.1-dir/shop.example.com"
nodo-tui --ctl expose e2e-app shop "${1:-0}" 0 1 shop.example.com
EOF
# Lets Pebble reach the device's ports 80 and 443 or not, as a router forwarding them would.
cat > /tmp/e2e-bin/pebble_ports <<'EOF'
#!/bin/sh
ip netns exec e2einet nft delete table inet acmetest 2>/dev/null
[ "$#" -eq 0 ] && exit 0
ip netns exec e2einet nft add table inet acmetest
ip netns exec e2einet nft add chain inet acmetest out '{ type filter hook output priority 0; }'
for port in "$@"; do
	ip netns exec e2einet nft add rule inet acmetest out ip saddr 203.0.113.1 ip daddr '{ 203.0.113.2, 93.184.216.34 }' tcp dport "$port" reject with tcp reset
done
EOF
cat > /tmp/e2e-bin/public_dns <<'EOF'
#!/bin/sh
curl -sf -d "{\"host\":\"$1.\"}" http://203.0.113.1:8055/clear-a >/dev/null &&
	curl -sf -d "{\"host\":\"$1.\",\"addresses\":[\"$2\"]}" http://203.0.113.1:8055/add-a >/dev/null
EOF
chmod 755 /tmp/e2e-bin/cert_state /tmp/e2e-bin/cert_block /tmp/e2e-bin/stored_fp /tmp/e2e-bin/served_fp \
	/tmp/e2e-bin/fresh_order /tmp/e2e-bin/pebble_ports /tmp/e2e-bin/public_dns

acme_down() {
	web_down
	ip netns exec e2einet true 2>/dev/null && ip netns pids e2einet | xargs -r kill 2>/dev/null
	rm -f /etc/resolv.conf.head
	[ -f "$acme/resolv.conf.saved" ] && cat "$acme/resolv.conf.saved" > /etc/resolv.conf
	sed -i '/# nodo-acme-test$/d' /etc/hosts
	rm -f /usr/local/share/ca-certificates/nodo-acme-test.crt
	update-ca-certificates >/dev/null 2>&1
}

acme_tests() {
	# The internet side: Pebble on 203.0.113.1, the fake provider and resolver on 203.0.113.10.
	web_down
	ip netns add e2elan; ip netns add e2einet
	ip link add e2elan0 type veth peer name eth0 netns e2elan
	ip link add e2einet0 type veth peer name eth0 netns e2einet
	ip addr add 192.168.77.2/24 dev e2elan0; ip addr add 203.0.113.2/24 dev e2einet0
	ip link set e2elan0 up; ip link set e2einet0 up
	ip netns exec e2elan ip addr add 192.168.77.1/24 dev eth0
	ip netns exec e2einet ip addr add 203.0.113.1/24 dev eth0
	ip netns exec e2einet ip addr add 203.0.113.10/24 dev eth0
	for ns in e2elan e2einet; do ip netns exec "$ns" ip link set lo up; ip netns exec "$ns" ip link set eth0 up; done
	ip netns exec e2einet ip route add 192.168.77.0/24 via 203.0.113.2
	# The home's public address, which the internet side routes to the device as a router forwarding
	# its ports would. IP echo services report it; a documentation address would count as no answer.
	ip addr add 93.184.216.34/32 dev e2einet0
	ip netns exec e2einet ip route add 93.184.216.34/32 via 203.0.113.2
	printf 'MODE=static\nPARENT=e2elan0\nADDRESS=192.168.77.3/24\n' > /etc/sv/nodo-localnet/conf
	HOME=/tmp/e2e-up XDG_DATA_HOME=/tmp/e2e-up nohup chpst -u nobody nodo-caddy run --config /tmp/e2e-up/config.json >/tmp/e2e-up/log 2>&1 </dev/null &

	mkdir -p "$acme/pki"
	rm -f "$acme/api.log" "$acme/hide-private"
	(
		cd "$acme/pki" || exit 1
		openssl req -x509 -newkey ec -pkeyopt ec_paramgen_curve:P-256 -nodes -days 2 -subj "/CN=nodo ACME test CA" \
			-keyout ca.key -out ca.crt 2>/dev/null
		for leaf in pebble api; do
			case $leaf in
			pebble) printf 'subjectAltName=IP:203.0.113.1\n' > ext ;;
			api) printf 'subjectAltName=DNS:api.cloudflare.com' > ext
				for h in $ECHO_HOSTS; do printf ',DNS:%s' "$h" >> ext; done; printf '\n' >> ext ;;
			esac
			openssl req -newkey ec -pkeyopt ec_paramgen_curve:P-256 -nodes -subj "/CN=$leaf" -keyout "$leaf.key" -out "$leaf.csr" 2>/dev/null
			openssl x509 -req -in "$leaf.csr" -CA ca.crt -CAkey ca.key -CAcreateserial -days 2 -extfile ext -out "$leaf.crt" 2>/dev/null
		done
	)
	cp "$acme/pki/ca.crt" /usr/local/share/ca-certificates/nodo-acme-test.crt 2>/dev/null ||
		{ mkdir -p /usr/local/share/ca-certificates; cp "$acme/pki/ca.crt" /usr/local/share/ca-certificates/nodo-acme-test.crt; }
	update-ca-certificates >/dev/null 2>&1
	sed -i '/# nodo-acme-test$/d' /etc/hosts
	echo "203.0.113.10 api.cloudflare.com $ECHO_HOSTS # nodo-acme-test" >> /etc/hosts
	[ -f "$acme/resolv.conf.saved" ] || cp /etc/resolv.conf "$acme/resolv.conf.saved"
	# Written in place, so sandboxes that bind the file see the change. dhcpcd rewrites the file
	# when interfaces come and go, and keeps the head file's lines when it does.
	echo "nameserver 203.0.113.10" > /etc/resolv.conf.head
	echo "nameserver 203.0.113.10" > /etc/resolv.conf

	# Certificates last three minutes, so renewal can be watched. Pebble never reuses an
	# authorization here, so every renewal solves a challenge again.
	cat > "$acme/pebble.json" <<'JSON'
{"pebble": {"listenAddress": "203.0.113.1:443", "managementListenAddress": "203.0.113.1:15000",
 "certificate": "/tmp/acme/pki/pebble.crt", "privateKey": "/tmp/acme/pki/pebble.key",
 "httpPort": 80, "tlsPort": 443, "ocspResponderURL": "", "externalAccountBindingRequired": false,
 "retryAfter": {"authz": 1, "order": 1}, "keyAlgorithm": "ecdsa",
 "profiles": {"default": {"description": "Short-lived, for renewal tests", "validityPeriod": 180}}}}
JSON
	ip netns exec e2einet nohup "$acme/pebble-challtestsrv" -dnsserver 203.0.113.1:53 -management 203.0.113.1:8055 \
		-defaultIPv4 "" -defaultIPv6 "" -http01 "" -https01 "" -tlsalpn01 "" -doh "" >"$acme/challtestsrv.log" 2>&1 </dev/null &
	PEBBLE_VA_NOSLEEP=1 PEBBLE_WFE_NONCEREJECT=0 PEBBLE_AUTHZREUSE=0 ip netns exec e2einet nohup "$acme/pebble" \
		-config "$acme/pebble.json" -dnsserver 203.0.113.1:53 >"$acme/pebble.log" 2>&1 </dev/null &
	ip netns exec e2einet nohup python3 "$acme/fake-dns.py" 203.0.113.10 "$acme/pki/api.crt" "$acme/pki/api.key" \
		"$CF_TOKEN" 203.0.113.1 93.184.216.34 "$acme" >"$acme/fake.log" 2>&1 </dev/null &
	sleep 3
	check "Pebble, challtestsrv and the fake provider are up, over the test CA" sh -c '
		curl -sf https://203.0.113.1/dir | grep -q newOrder &&
		curl -sf --cacert /tmp/acme/pki/ca.crt https://203.0.113.1:15000/roots/0 > /tmp/acme/pebble-roots.pem &&
		grep -q "BEGIN CERTIFICATE" /tmp/acme/pebble-roots.pem && [ "$(curl -sf https://api.ipify.org/)" = 93.184.216.34 ]'
	# Pebble's intermediates sign with a new root at each start, so curl gets that root.

	# The helper uses Pebble instead of Let's Encrypt. Caddy reads the system CAs when it starts.
	printf 'directory = "https://203.0.113.1/dir"\nrenew_interval_secs = 10\n' > /etc/nodo/acme-test.toml
	cp /var/lib/nodo/secrets/web-dns.toml "$acme/web-dns.saved" 2>/dev/null
	# A backoff left by the web section's unreachable provider would delay dynamic DNS here.
	rm -f /var/lib/nodo/secrets/web-dns.toml /var/lib/nodo/secrets/web-acme.toml "$tls_log" /var/lib/nodo-ddns/state.json
	rm -rf "$issuer_dir" "$caddy_store/acme/203.0.113.1-dir"
	[ -L /var/service/nodo-caddy ] && sv -w 15 restart nodo-caddy >/dev/null 2>&1

	check "an invalid ACME email is refused" refused "not valid" ctl acme-set "not an email" 0
	check "the ACME email is stored root only and the test server is named" sh -c '
		ctl acme-set e2e@example.com 0 && [ "$(stat -c "%a %U" /var/lib/nodo/secrets/web-acme.toml)" = "600 root" ] &&
		ctl acme | tee /tmp/e2e.acme; grep -q "test_server: true" /tmp/e2e.acme && grep -q "https://203.0.113.1/dir" /tmp/e2e.acme &&
		grep -q "PortsNeeded" /tmp/e2e.acme'

	# TLS-ALPN-01: the router forwards 443 only.
	public_dns shop.example.com 93.184.216.34
	pebble_ports 80
	ctl expose e2e-app shop 0 0 1 shop.example.com > /tmp/e2e.ex 2>&1
	ctl certs > /tmp/e2e.first 2>&1
	check "going public reports the certificate as being obtained, not as done" sh -c '
		cat /tmp/e2e.ex /tmp/e2e.first; grep -q "host: \"shop.example.com\"" /tmp/e2e.first &&
		! grep -q "state: Valid" /tmp/e2e.first'
	check "TLS-ALPN-01 gets a real certificate from Pebble" sh -c '
		cert_state shop.example.com Valid 120 && cert_block shop.example.com | grep -q "challenge: Some(\"tls-alpn-01\"" &&
		grep -q "\"email\": \"e2e@example.com\"" /var/lib/nodo-web/caddy.json'
	check "the certificate in place is the one Caddy stored, from Pebble" sh -c '
		cert_block shop.example.com | tee /tmp/e2e.cb; grep -q "issuer: Some(\"Pebble Intermediate" /tmp/e2e.cb &&
		grep -q "not_after: Some(" /tmp/e2e.cb && grep -q "last_renewal: Some(" /tmp/e2e.cb &&
		[ -n "$(stored_fp shop.example.com)" ] && [ "$(served_fp e2einet 203.0.113.2 shop.example.com)" = "$(stored_fp shop.example.com)" ]'
	check "Caddy's storage stays private to Caddy" sh -c '
		[ "$(stat -c "%a %U" /var/lib/caddy)" = "700 _caddy" ] && [ "$(stat -c %U /var/lib/caddy/data/caddy/certificates)" = _caddy ] &&
		[ "$(stat -c "%a" /var/lib/caddy/data/caddy/certificates/203.0.113.1-dir/shop.example.com/shop.example.com.key)" = 600 ]'

	# HTTP-01: the router forwards 80 only.
	pebble_ports 443
	fresh_order >/dev/null
	check "HTTP-01 gets a real certificate from Pebble" sh -c '
		cert_state shop.example.com Valid 120 && cert_block shop.example.com | grep -q "challenge: Some(\"http-01\""'

	# A failed challenge: no port is forwarded.
	pebble_ports 80 443
	fresh_order >/dev/null
	check "a failed challenge is reported with both types and the ports" sh -c '
		cert_state shop.example.com Failed 120 && cert_block shop.example.com | tee /tmp/e2e.cb;
		grep -q "ChallengeFailed" /tmp/e2e.cb && grep -q "\"http-01\"" /tmp/e2e.cb && grep -q "\"tls-alpn-01\"" /tmp/e2e.cb &&
		grep -q "ports 80 and 443" /tmp/e2e.cb && ! grep -q "state: Valid" /tmp/e2e.cb'

	# DNS that points somewhere else, here at the fake provider's address.
	pebble_ports
	public_dns shop.example.com 203.0.113.10
	fresh_order >/dev/null
	check "DNS not pointing at the device is named, with both addresses" sh -c '
		cert_state shop.example.com Failed 120 && cert_block shop.example.com | tee /tmp/e2e.cb;
		grep -q "DnsNotPointingHere" /tmp/e2e.cb && grep -q "points to 203.0.113.10, not to this device" /tmp/e2e.cb &&
		grep -q "93.184.216.34" /tmp/e2e.cb'
	public_dns shop.example.com 93.184.216.34

	# DNS-01 through caddy-dns/cloudflare and the fake API, with no port forwarded at all.
	pebble_ports 80 443
	ctl expose e2e-app shop 0 0 0 >/dev/null
	check "storing Cloudflare switches the challenge to DNS-01" sh -c '
		echo "$0" | nodo-tui --ctl set-dns example.com cloudflare api_token &&
		ctl acme | tr -d " \\n" | grep -q "Dns01{provider:\"Cloudflare\""' "$CF_TOKEN"
	fresh_order >/dev/null
	check "DNS-01 gets a real certificate with no inbound port" sh -c '
		cert_state shop.example.com Valid 150 && cert_block shop.example.com | grep -q "challenge: Some(\"dns-01\"" &&
		grep -q "\"record\": \"_acme-challenge.shop.example.com\", \"type\": \"TXT\", \"values\": \[\"" /tmp/acme/api.log &&
		grep -q "\"DELETE\"" /tmp/acme/api.log'
	check "the Caddy config holding the DNS-01 token is root:_caddy 640" sh -c '
		[ "$(stat -c "%a %U %G" /var/lib/nodo-web/caddy.json)" = "640 root _caddy" ] &&
		grep -q "\"api_token\": \"$0\"" /var/lib/nodo-web/caddy.json && ! grep -q "$0" /var/lib/nodo/secrets/web-dns.toml &&
		! ctl certs | grep -q "$0" && ! grep -q "$0" /var/lib/caddy/nodo-tls.log' "$CF_TOKEN"
	check "dynamic DNS published the public address through the same provider" sh -c '
		grep "\"POST\"" /tmp/acme/api.log | grep "\"type\": \"A\"" | grep -q "93.184.216.34"'
	first_fp=$(stored_fp shop.example.com)
	first_at=$(date +%s)

	# State survives a restart: Caddy serves what it stored and does not order again.
	mark=$(wc -l < "$tls_log")
	sv -w 30 restart nodo-caddy >/dev/null
	sleep 3
	check "after a Caddy restart the stored certificate is served without a new order" sh -c '
		[ "$(served_fp e2einet 203.0.113.2 shop.example.com)" = "$(stored_fp shop.example.com)" ] &&
		! tail -n "+$(($0 + 1))" /var/lib/caddy/nodo-tls.log | grep -q "\"msg\":\"obtaining certificate\""' "$mark"

	# And a backup and restore onto storage that lost it.
	backup_fp=$(stored_fp shop.example.com)
	printf '%s\n%s\n' "$PASS2" 'acme backup passphrase' | nodo-tui --ctl backup > /tmp/e2e.abk 2>&1
	acme_bk=$(sed -n 's/.*name: "\(nodo-[^"]*\)".*/\1/p' /tmp/e2e.abk | head -1)
	touch /etc/sv/nodo-caddy/down; sv -w 30 down nodo-caddy >/dev/null
	rm -rf "$caddy_store/certificates" "$caddy_store/acme"
	check "a restore brings Caddy's certificates and ACME account back, private to Caddy" sh -c '
		printf "%s\n%s\n" "acme backup passphrase" "new device pass" | nodo-tui --ctl restore "$0" restore | tee /tmp/e2e.ars
		grep -q "\"certificates\"" /tmp/e2e.ars && [ "$(stored_fp shop.example.com)" = "$1" ] &&
		ls /var/lib/caddy/data/caddy/acme/203.0.113.1-dir/users/*/*.key >/dev/null &&
		[ "$(stat -c %U /var/lib/caddy/data/caddy/certificates/203.0.113.1-dir/shop.example.com/shop.example.com.key)" = _caddy ]' \
		"$acme_bk" "$backup_fp"
	printf '%s\n' "$PASS2" | nodo-tui --ctl unlock >/dev/null
	mark=$(wc -l < "$tls_log")
	rm -f /etc/sv/nodo-caddy/down; sv -w 30 up nodo-caddy >/dev/null
	sleep 3
	check "the restored certificate is served without a new order" sh -c '
		[ "$(served_fp e2einet 203.0.113.2 shop.example.com)" = "$(stored_fp shop.example.com)" ] &&
		! tail -n "+$(($0 + 1))" /var/lib/caddy/nodo-tls.log | grep -q "\"msg\":\"obtaining certificate\""' "$mark"
	rm -f "/var/lib/nodo/backups/$acme_bk"

	# Renewal: the three-minute certificate is replaced through DNS-01 before it runs out.
	check "a short-lived certificate is renewed and the new one served" sh -c '
		for _ in $(seq 1 200); do
			now=$(stored_fp shop.example.com)
			if [ -n "$now" ] && [ "$now" != "$0" ] && grep -q "certificate renewed successfully" /var/lib/caddy/nodo-tls.log &&
				[ "$(served_fp e2einet 203.0.113.2 shop.example.com)" = "$now" ]; then
				cert_block shop.example.com | grep -q "state: Valid" && exit 0
			fi
			sleep 2
		done
		grep -c renew /var/lib/caddy/nodo-tls.log; exit 1' "$first_fp"

	# A local site named under the domain: DNS-01, an A record to the local address, LAN only.
	check "local names under the domain are refused outside the zone" refused "under example.com" ctl acme-set e2e@example.com 0 home.example.org
	check "local names under the domain can be chosen with DNS-01" ctl acme-set e2e@example.com 0 home.example.com
	ctl expose e2e-app shop 1 0 1 >/dev/null
	check "the local site under the domain gets a Pebble certificate through DNS-01" sh -c '
		grep -qx "address=192.168.77.3" /run/nodo-localnet/state &&
		cert_state shop.home.example.com Valid 150 && cert_block shop.home.example.com | tee /tmp/e2e.cb;
		grep -q "kind: LocalDomain" /tmp/e2e.cb && grep -q "challenge: Some(\"dns-01\"" /tmp/e2e.cb'
	check "its A record holds the local address" sh -c '
		grep "\"POST\"" /tmp/acme/api.log | grep "shop.home.example.com" | grep -q "192.168.77.3" &&
		ctl local-names | tee /tmp/e2e.ln; grep -q "verdict: Resolves" /tmp/e2e.ln'
	check "it is served on the local address to the LAN" sh -c '
		ip netns exec e2elan curl -sf --cacert /tmp/acme/pebble-roots.pem --resolve shop.home.example.com:443:192.168.77.3 \
			https://shop.home.example.com/admin | grep -q "e2e shop /admin"'
	check "it is not served on the main address, nor to the internet" sh -c '
		! ip netns exec e2elan curl -s -k --max-time 6 --resolve shop.home.example.com:443:192.168.77.2 https://shop.home.example.com/ | grep -q "e2e shop" &&
		! ip netns exec e2einet curl -s -k --max-time 6 --interface 203.0.113.10 --resolve shop.home.example.com:443:203.0.113.2 https://shop.home.example.com/ | grep -q "e2e shop" &&
		! ip netns exec e2einet curl -s -k --max-time 6 --resolve shop.home.example.com:443:192.168.77.3 https://shop.home.example.com/ | grep -q "e2e shop"'
	touch "$acme/hide-private"
	check "a resolver that drops private answers is named as DNS rebinding protection" sh -c '
		ctl local-names | tee /tmp/e2e.ln; grep -q "verdict: Blocked" /tmp/e2e.ln && grep -q "rebinding" /tmp/e2e.ln'
	rm -f "$acme/hide-private"
	check "back to .local, the record revealing the LAN address is removed" sh -c '
		ctl acme-set e2e@example.com 0 &&
		grep "\"record\": \"shop.home.example.com\"" /tmp/acme/api.log | tail -n 1 | grep -q "\"values\": \[\]" &&
		grep -q "shop.local" /var/lib/nodo-web/caddy.json && ! grep -q "shop.home.example.com" /var/lib/nodo-web/caddy.json'

	ctl expose e2e-app shop 0 0 0 >/dev/null
	rm -f /var/lib/nodo/secrets/web-acme.toml
	if [ -f "$acme/web-dns.saved" ]; then
		cp "$acme/web-dns.saved" /var/lib/nodo/secrets/web-dns.toml
		chmod 600 /var/lib/nodo/secrets/web-dns.toml
	fi
	acme_down
}

if [ -x "$acme/pebble" ] && [ -x "$acme/pebble-challtestsrv" ] && command -v python3 >/dev/null && command -v curl >/dev/null; then
	acme_tests
else
	echo "SKIP acme: run tests/acme-prepare.sh to put Pebble and its fakes in the VM"
fi
