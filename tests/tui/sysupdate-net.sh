#!/bin/sh
# A pretend update server for the Updates journey's system base row, as tests/snapshot-update.sh
# builds it, all inside the VM, which stays offline: Caddy in a network namespace at 198.18.1.1
# serves the test snapshots tests/snapshot-prepare.sh put in /srv/snapshot-test, over HTTPS with a
# throwaway CA, as https://updates.example.test. The device is moved to snapshot test-a with
# nodo-snaptest 1.0 running, and the channel then offers test-b, which updates nodo-snaptest.
#   sysupdate-net.sh up     builds it; prints "ready"
#   sysupdate-net.sh down   removes it and puts the image's signing keys and CA list back
set -u
ns=tuiupd
peer=198.18.1.1
src=/srv/snapshot-test
www=$src/server
state=/tmp/tui-sysupdate
pins=/usr/share/nodo/snapshot-pins.toml
host=updates.example.test

offer() {
	mkdir -p "$www/channels/test"
	cp "$www/snapshots/$1/manifest.toml" "$www/snapshots/$1/manifest.toml.sig" "$www/channels/test/"
}

down() {
	rm -f /etc/nodo/system-update.toml /var/service/nodo-snaptest
	ip netns pids "$ns" 2>/dev/null | xargs -r kill 2>/dev/null
	ip netns del "$ns" 2>/dev/null
	ip link del tuiupd0 2>/dev/null
	sed -i '/# nodo-tui-update$/d' /etc/hosts
	rm -f /usr/local/share/ca-certificates/nodo-tui-update.crt
	update-ca-certificates >/dev/null 2>&1
	if [ -f "$state/pins.saved" ]; then cp "$state/pins.saved" "$pins"; rm -f "$state/pins.saved"; fi
}

up() {
	[ -s "$www/snapshots/test-b/manifest.toml.sig" ] || { echo "no test snapshots: run tests/snapshot-prepare.sh"; exit 1; }
	down
	mkdir -p "$state"
	[ -f "$pins" ] && cp "$pins" "$state/pins.saved"
	ip netns add "$ns"
	ip link add tuiupd0 type veth peer name eth0 netns "$ns"
	ip addr add 198.18.1.2/24 dev tuiupd0
	ip link set tuiupd0 up
	ip netns exec "$ns" ip addr add "$peer/24" dev eth0
	ip netns exec "$ns" ip link set lo up
	ip netns exec "$ns" ip link set eth0 up
	rm -rf "$state/caddy" "$www/channels"
	mkdir -p "$state/caddy"
	cat > "$state/caddy/config.json" <<-JSON
	{"admin":{"disabled":true},"logging":{"logs":{"default":{"level":"ERROR"}}},
	 "apps":{
	  "tls":{"automation":{"policies":[{"issuers":[{"module":"internal"}]}]}},
	  "pki":{"certificate_authorities":{"local":{"install_trust":false}}},
	  "http":{"servers":{"tls":{"listen":["$peer:443"],"automatic_https":{"disable_redirects":true},"routes":[
	    {"match":[{"host":["$host"]}],"handle":[{"handler":"file_server","root":"$www"}]}]}}}}}
	JSON
	XDG_DATA_HOME=$state/caddy XDG_CONFIG_HOME=$state/caddy HOME=$state/caddy \
		nohup ip netns exec "$ns" nodo-caddy run --config "$state/caddy/config.json" >"$state/caddy.log" 2>&1 </dev/null &
	i=0; until [ -s "$state/caddy/caddy/pki/authorities/local/root.crt" ] || [ "$i" -ge 20 ]; do i=$((i+1)); sleep 1; done
	mkdir -p /usr/local/share/ca-certificates
	cp "$state/caddy/caddy/pki/authorities/local/root.crt" /usr/local/share/ca-certificates/nodo-tui-update.crt
	update-ca-certificates >/dev/null 2>&1
	echo "$peer $host # nodo-tui-update" >> /etc/hosts
	# The image's keys are replaced by the test snapshots' throwaway ones until down.
	install -D -m 644 "$src/pins.toml" "$pins"
	rm -f /var/lib/nodo/update/system-trust.toml /var/lib/nodo/updates/system.toml
	xbps-query nodo-snaptest >/dev/null 2>&1 && xbps-remove -y nodo-snaptest >/dev/null
	rm -f /var/cache/xbps/nodo-snaptest-*
	xbps-rindex -a "$src"/local/*.xbps >/dev/null 2>&1
	xbps-install -y -R "$src/local" nodo-snaptest >/dev/null || { echo "nodo-snaptest 1.0 did not install"; exit 1; }
	ln -sfn /etc/sv/nodo-snaptest /var/service/nodo-snaptest
	sleep 2
	offer test-a
	nodo-sysupdate apply --server "https://$host" >"$state/apply-a.log" 2>&1 || { cat "$state/apply-a.log"; exit 1; }
	offer test-b
	mkdir -p /etc/nodo
	printf 'server = "https://%s"\n' "$host" > /etc/nodo/system-update.toml
	echo ready
}

case ${1:?usage: sysupdate-net.sh up|down} in
up) up ;;
down) down ;;
*) echo "usage: sysupdate-net.sh up|down" >&2; exit 2 ;;
esac
