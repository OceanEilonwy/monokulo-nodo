#!/bin/sh
# A signed test feed for tests/tui/journeys.py, built in the VM with xbps-create and xbps-rindex.
#   feed.sh init                 new keys and an empty feed at /srv/tui-feed
#   feed.sh pkg VERSION [MODE]   builds and publishes tui-shop at VERSION; MODE "crash" fails its health check
#   feed.sh resign               signs the feed again with the second key, as a key change would
set -eu
FEED=/srv/tui-feed
WORK=/tmp/tui-pkgs
KEYS=/root/tui-feed-keys

publish() {
	key=$1 file=$2
	[ "$(dirname "$file")" = "$FEED" ] || cp "$file" "$FEED/"
	xbps-rindex -f -a "$FEED/$(basename "$file")" >/dev/null
	xbps-rindex --signedby tui-test --privkey "$key" -s "$FEED" >/dev/null
	xbps-rindex --privkey "$key" -S "$FEED/$(basename "$file")" >/dev/null
}

case ${1:?usage: feed.sh init|pkg|resign} in
init)
	rm -rf "$FEED" "$WORK" "$KEYS"
	mkdir -p "$FEED" "$WORK" "$KEYS"
	openssl genrsa -out "$KEYS/first.pem" 2048 2>/dev/null
	openssl genrsa -out "$KEYS/second.pem" 2048 2>/dev/null
	cp "$KEYS/first.pem" "$KEYS/current.pem"
	;;
pkg)
	ver=$2 mode=${3:-good}
	d=$WORK/tui-shop-$ver
	app=$d/opt/nodo/apps/tui-shop
	rm -rf "$d"
	mkdir -p "$app/bin"
	chmod 755 "$d" "$d/opt" "$d/opt/nodo" "$d/opt/nodo/apps" "$app" "$app/bin"
	cat > "$app/manifest.toml" <<-EOF
	api_version = 1
	id = "tui-shop"
	version = "$ver"
	exec = ["bin/run"]
	settings = "settings.lua"
	stop_timeout_secs = 5

	[storage]
	encrypted = true

	[network]
	outbound = "internet"

	[[web]]
	name = "shop"
	upstream = "127.0.0.1:8090"
	suggested_subdomain = "shop"
	multi_address = true
	EOF
	cat > "$app/settings.lua" <<-'EOF'
	return { api_version = 1, settings = {
	  { name = "workers", type = "int", label = "Worker threads", default = 2, min = 1, max = 16,
	    help = "Requests handled at once." },
	  { name = "greeting", type = "string", label = "Greeting", default = "hello",
	    validate = function(v) return #v <= 12, "must be 12 characters or fewer" end },
	} }
	EOF
	if [ "$mode" = good ]; then
		printf '#!/bin/sh\necho "tui-shop %s running"\nwhile :; do sleep 1; done\n' "$ver" > "$app/bin/run"
	else
		printf '#!/bin/sh\necho "migration 7: column already exists" >&2\nexit 1\n' > "$app/bin/run"
	fi
	chmod 644 "$app/manifest.toml" "$app/settings.lua"
	chmod 755 "$app/bin/run"
	(cd "$WORK" && xbps-create -A x86_64 -n "tui-shop-${ver}_1" -s "A small shop for the TUI test" "$d" >/dev/null)
	publish "$KEYS/current.pem" "$WORK/tui-shop-${ver}_1.x86_64.xbps"
	;;
resign)
	cp "$KEYS/second.pem" "$KEYS/current.pem"
	for f in "$FEED"/*.xbps; do publish "$KEYS/current.pem" "$f"; done
	;;
esac
