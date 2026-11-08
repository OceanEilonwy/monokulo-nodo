#!/bin/sh
# A pretend LAN and internet for the Web journey, as tests/acme-e2e.sh builds them, all inside the
# VM, which stays offline. The device's main address is 192.168.77.2 and its local address
# 192.168.77.3. The internet side holds the fake Cloudflare API and IP echo services at
# 203.0.113.10, behind /etc/hosts and a test CA, and, when tests/acme-prepare.sh has put Pebble at
# /tmp/acme, Pebble and pebble-challtestsrv at 203.0.113.1.
#   web-net.sh up TOKEN   builds it; prints "pebble" when Pebble runs
#   web-net.sh down       removes it and restores the resolver and CA list
set -u
here=$(cd "$(dirname "$0")" && pwd)
state=/tmp/tui-web
acme=/tmp/acme
ECHO_HOSTS="api.ipify.org ipv4.icanhazip.com checkip.amazonaws.com v4.ident.me ipv4.wtfismyip.com"

down() {
	for s in nodo-caddy nodo-mdns nodo-localnet nodo-ddns; do
		[ -d "/etc/sv/$s" ] && { touch "/etc/sv/$s/down"; sv -w 15 down "$s" >/dev/null 2>&1; }
	done
	for ns in tuilan tuinet; do ip netns pids "$ns" 2>/dev/null | xargs -r kill 2>/dev/null; ip netns del "$ns" 2>/dev/null; done
	ip link del tuilan0 2>/dev/null; ip link del tuinet0 2>/dev/null
	rm -f /etc/nftables.d/nodo-web.nft /etc/sv/nodo-localnet/conf /etc/resolv.conf.head /etc/nodo/acme-test.toml
	[ -f "$state/resolv.conf.saved" ] && cat "$state/resolv.conf.saved" > /etc/resolv.conf
	sed -i '/# nodo-tui-test$/d' /etc/hosts
	rm -f /usr/local/share/ca-certificates/nodo-tui-test.crt
	update-ca-certificates >/dev/null 2>&1
	nft -f /etc/nftables.conf
}

up() {
	token=$1
	down
	mkdir -p "$state/pki"
	ip netns add tuilan; ip netns add tuinet
	ip link add tuilan0 type veth peer name eth0 netns tuilan
	ip link add tuinet0 type veth peer name eth0 netns tuinet
	ip addr add 192.168.77.2/24 dev tuilan0; ip addr add 203.0.113.2/24 dev tuinet0
	ip link set tuilan0 up; ip link set tuinet0 up
	ip netns exec tuilan ip addr add 192.168.77.1/24 dev eth0
	ip netns exec tuinet ip addr add 203.0.113.1/24 dev eth0
	ip netns exec tuinet ip addr add 203.0.113.10/24 dev eth0
	for ns in tuilan tuinet; do ip netns exec "$ns" ip link set lo up; ip netns exec "$ns" ip link set eth0 up; done
	ip netns exec tuinet ip route add 192.168.77.0/24 via 203.0.113.2
	# The home's public address, routed to the device as a router forwarding its ports would.
	ip addr add 93.184.216.34/32 dev tuinet0
	ip netns exec tuinet ip route add 93.184.216.34/32 via 203.0.113.2
	printf 'MODE=static\nPARENT=tuilan0\nADDRESS=192.168.77.3/24\n' > /etc/sv/nodo-localnet/conf
	(
		cd "$state/pki" || exit 1
		openssl req -x509 -newkey ec -pkeyopt ec_paramgen_curve:P-256 -nodes -days 2 -subj "/CN=nodo TUI test CA" \
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
	mkdir -p /usr/local/share/ca-certificates
	cp "$state/pki/ca.crt" /usr/local/share/ca-certificates/nodo-tui-test.crt
	update-ca-certificates >/dev/null 2>&1
	echo "203.0.113.10 api.cloudflare.com $ECHO_HOSTS # nodo-tui-test" >> /etc/hosts
	[ -f "$state/resolv.conf.saved" ] || cp /etc/resolv.conf "$state/resolv.conf.saved"
	echo "nameserver 203.0.113.10" > /etc/resolv.conf.head
	echo "nameserver 203.0.113.10" > /etc/resolv.conf
	mkdir -p /etc/nodo
	printf 'directory = "https://203.0.113.1/dir"\nrenew_interval_secs = 10\n' > /etc/nodo/acme-test.toml
	if [ -x "$acme/pebble" ] && [ -x "$acme/pebble-challtestsrv" ]; then
		cat > "$state/pebble.json" <<-JSON
		{"pebble": {"listenAddress": "203.0.113.1:443", "managementListenAddress": "203.0.113.1:15000",
		 "certificate": "$state/pki/pebble.crt", "privateKey": "$state/pki/pebble.key",
		 "httpPort": 80, "tlsPort": 443, "ocspResponderURL": "", "externalAccountBindingRequired": false,
		 "retryAfter": {"authz": 1, "order": 1}, "keyAlgorithm": "ecdsa"}}
		JSON
		ip netns exec tuinet nohup "$acme/pebble-challtestsrv" -dnsserver 203.0.113.1:53 -management 203.0.113.1:8055 \
			-defaultIPv4 "" -defaultIPv6 "" -http01 "" -https01 "" -tlsalpn01 "" -doh "" >"$state/challtestsrv.log" 2>&1 </dev/null &
		PEBBLE_VA_NOSLEEP=1 PEBBLE_WFE_NONCEREJECT=0 ip netns exec tuinet nohup "$acme/pebble" \
			-config "$state/pebble.json" -dnsserver 203.0.113.1:53 >"$state/pebble.log" 2>&1 </dev/null &
		echo pebble
	fi
	ip netns exec tuinet nohup python3 "$here/fake-dns.py" 203.0.113.10 "$state/pki/api.crt" "$state/pki/api.key" \
		"$token" 203.0.113.1 93.184.216.34 "$state" >"$state/fake.log" 2>&1 </dev/null &
	for _ in $(seq 1 20); do
		[ "$(curl -sf https://api.ipify.org/ 2>/dev/null)" = 93.184.216.34 ] && return 0
		sleep 0.5
	done
	echo "the fake internet did not come up" >&2
	cat "$state/fake.log" >&2
	return 1
}

case ${1:?usage: web-net.sh up TOKEN | down} in
up) up "${2:?a token}" ;;
down) down ;;
esac
