#!/bin/sh
# Builds a signed app feed, in explicit steps. Each app's release workflow runs sign and check
# (.github/workflows/app-release.yml, which attaches the feed to a GitHub release); another
# project can also publish a feed as a static site. docs/FEED.md explains feeds and their keys.
#
#   scripts/publish-feed.sh build [arch...]     build the recipes in $FEED_PACKAGES (default aarch64)
#   scripts/publish-feed.sh sign <key> [arch...] copy them into $FEED_SITE/feed, index and sign them
#   scripts/publish-feed.sh check [arch...]     verify $FEED_SITE/feed as a device would
#   scripts/publish-feed.sh site [arch...]      write a landing page around it, for a static site
#
# The feed, under $FEED_SITE (default out/site), and the site around it:
#
#   feed/<arch>-repodata     the xbps index, signed: the key and signer are in its meta
#   feed/<name>-<ver>.<arch>.xbps and .xbps.sig2
#   index.html               (site) what the feed is, its address, key fingerprint and packages
#   CNAME                    (site) the address's host, for GitHub Pages
#   404.html                 (site) so a missing file is answered 404, not 503
#   signing-key.pub.pem      (site) the public key, to compare with what a device shows
#
# nodo-install fetches <address>/<arch>-repodata, <address>/<file>.xbps and
# <address>/<file>.xbps.sig2. Every architecture shares the one directory, as xbps allows, since
# file names carry the arch.
#
# <key> is an RSA private key in PEM form. It is mounted read-only into a throwaway container
# and never copied; xbps-rindex signs the index meta (-s) and each package (-S, a .sig2: RSA
# PKCS#1 v1.5 over the SHA-256 of the file), which is what nodo-feed verifies.
set -eu
. "$(dirname "$0")/lib.sh"
cmd=${1:?usage: publish-feed.sh build|sign|check|site ...}
shift
site=${FEED_SITE:-$root/out/site}
mkdir -p "$site"
site=$(cd "$site" && pwd)
# The feed is described by:
#   FEED_PACKAGES     the package list, one name per line (required); a subpackage is listed
#                     beside its recipe and built with it
#   FEED_SIGNER       the signer name in the index (default "monokulo-nodo feed")
#   FEED_FINGERPRINT  the key fingerprint `check` requires, as xbps shows it (check and site)
#   FEED_ADDRESS      the feed's https address, for the landing page (site)
#   FEED_PUBLIC_KEY   the signing key's public half, PEM, copied onto the landing page (site)
[ -n "${FEED_PACKAGES:-}" ] || { echo "set FEED_PACKAGES to the feed's package list" >&2; exit 2; }
apps=$(sed -e 's/#.*//' -e '/^[[:space:]]*$/d' "$FEED_PACKAGES")
need_fingerprint() {
	fingerprint=${FEED_FINGERPRINT:-}
	case $fingerprint in *[!0-9a-fA-F:]* | '') echo "bad or missing FEED_FINGERPRINT: $fingerprint" >&2; exit 2 ;; esac
}
signer=${FEED_SIGNER:-monokulo-nodo feed}
case $signer in *[!A-Za-z0-9\ ._-]* | '') echo "bad FEED_SIGNER: $signer" >&2; exit 2 ;; esac
plain() { case $1 in *[!A-Za-z0-9._-]* | .* | -* | '') echo "bad $2: $1" >&2; exit 2 ;; esac; }
archs() { if [ $# -gt 0 ]; then for a in "$@"; do plain "$a" architecture; done; echo "$@"; else echo aarch64; fi; }

case $cmd in
build)
	recipes=
	for app in $apps; do is_subpackage "$app" || recipes="$recipes $app"; done
	# build-packages.sh with no package names builds the base system's, which is not this.
	[ -n "$recipes" ] || { echo "no recipes to build in $FEED_PACKAGES"; exit 0; }
	for arch in $(archs "$@"); do
		# shellcheck disable=SC2086 # one word per package
		"$root/scripts/build-packages.sh" "$arch" $recipes
	done
	;;
sign)
	key=${1:?the feed signing key, an RSA private key in PEM form}
	shift
	[ -f "$key" ] || { echo "no key file: $key" >&2; exit 1; }
	feed=$site/feed
	rm -rf "$feed"
	mkdir -p "$feed"
	list=$(archs "$@")
	for arch in $list; do
		for app in $apps; do
			# build-packages.sh keeps one version of each package in out/feed/<arch>.
			found=
			for f in "$root/out/feed/$arch/$app"-[0-9]*."$arch".xbps "$root/out/feed/$arch/$app"-[0-9]*.noarch.xbps; do
				[ -f "$f" ] || continue
				[ -z "$found" ] || { echo "more than one $app in out/feed/$arch" >&2; exit 1; }
				found=$f
			done
			[ -n "$found" ] || { echo "$app is not built for $arch: run publish-feed.sh build $arch" >&2; exit 1; }
			cp "$found" "$feed/"
		done
	done
	# The pinned Void image has xbps; nothing else is needed, so no builder image is built here.
	docker run --rm --network none -e "ARCHS=$list" -e "SIGNER=$signer" -e "HOST_UID=$(id -u)" -e "HOST_GID=$(id -g)" \
		-v "$feed:/feed" -v "$(realpath "$key"):/key.pem:ro" "$VOID_IMAGE" sh -euc '
			trap "chown -R $HOST_UID:$HOST_GID /feed" EXIT
			for arch in $ARCHS; do
				set --
				for f in /feed/*.$arch.xbps /feed/*.noarch.xbps; do [ -f "$f" ] && set -- "$@" "$f"; done
				XBPS_TARGET_ARCH=$arch xbps-rindex -a "$@" >/dev/null
				XBPS_TARGET_ARCH=$arch xbps-rindex --signedby "$SIGNER" --privkey /key.pem -s /feed >/dev/null
				XBPS_TARGET_ARCH=$arch xbps-rindex --privkey /key.pem -S "$@" >/dev/null
			done'
	echo "signed $(find "$feed" -name '*.xbps' | wc -l) packages for $list in $feed"
	;;
check)
	need_fingerprint
	for arch in $(archs "$@"); do
		(cd "$root" && cargo run -q --release -p nodo-install --example check-feed -- "$site/feed" "$arch" "$fingerprint") \
			> "$site/.check-$arch.tsv"
		echo "$arch: $(grep -c '^package' "$site/.check-$arch.tsv") packages pass the device's checks, key $fingerprint"
	done
	;;
site)
	list=$(archs "$@")
	esc() { sed -e 's/&/\&amp;/g' -e 's/</\&lt;/g' -e 's/>/\&gt;/g' -e 's/"/\&quot;/g'; }
	# A field from the app's recipe here, or, for an app built in its own repository, from the
	# package's own metadata (props.plist, the first file in every .xbps).
	field() {
		if dir=$(recipe_dir "$1" 2>/dev/null); then
			sed -n "s/^$2=\"\{0,1\}\([^\"]*\)\"\{0,1\}\$/\1/p" "$dir/template" | head -n 1
		else
			tar -xOf "$3" ./props.plist | awk -v k="<key>$2</key>" '
				found && /<string>/ { sub(/.*<string>/, ""); sub(/<\/string>.*/, ""); print; exit }
				index($0, k) { found = 1 }' |
				sed -e 's/&lt;/</g' -e 's/&gt;/>/g' -e 's/&quot;/"/g' -e "s/&apos;/'/g" -e 's/&amp;/\&/g'
		fi
	}
	need_fingerprint
	address=${FEED_ADDRESS:?set FEED_ADDRESS to the https address of the feed}
	case $address in https://*) ;; *) echo "FEED_ADDRESS must be an https address: $address" >&2; exit 2 ;; esac
	host=${address#https://}; host=${host%%/*}
	[ -f "${FEED_PUBLIC_KEY:-}" ] || { echo "set FEED_PUBLIC_KEY to the signing key's public half (PEM)" >&2; exit 2; }
	shown=$(printf '%s' "$fingerprint" | tr -d : | sed 's/../&:/g; s/:$//')
	printf '%s\n' "$host" > "$site/CNAME"
	# GitHub Pages answered a missing file inside a directory with HTTP 503 while the site had no
	# 404 page (2026-11-13). This page is meant to make that a 404. Devices do not rely on it.
	printf %s\\n "<!doctype html><title>Not found</title><p>Not found." > "$site/404.html"
	cp "$FEED_PUBLIC_KEY" "$site/signing-key.pub.pem"
	rows=
	for arch in $list; do
		[ -s "$site/.check-$arch.tsv" ] || { echo "run publish-feed.sh check $arch first" >&2; exit 1; }
		while IFS="$(printf '\t')" read -r kind name version size _; do
			[ "$kind" = package ] || continue
			file=$name-$version.$arch.xbps
			desc=$(field "$name" short_desc "$site/feed/$file" | esc)
			home=$(field "$name" homepage "$site/feed/$file" | esc)
			license=$(field "$name" license "$site/feed/$file" | esc)
			mb=$(awk -v b="$size" 'BEGIN { printf "%.1f", b / 1048576 }')
			rows="$rows<tr><td class=\"a\">$(printf %s "$name" | esc)</td><td>$(printf %s "$version" | esc)</td><td class=\"d\">$arch</td><td class=\"n\"><a href=\"feed/$file\">$mb MB</a></td><td>$desc<br><span class=\"d\">$license · <a href=\"$home\">source</a></span></td></tr>
"
		done < "$site/.check-$arch.tsv"
	done
	built=$(date -u +'%Y-%m-%d %H:%M UTC')
	commit=$(git -C "$root" rev-parse --short HEAD 2>/dev/null || echo unknown)
	cat > "$site/index.html" <<EOF
<!doctype html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>monokulo-nodo feed</title>
<meta name="description" content="The signed app feed for monokulo-nodo devices.">
<link rel="preconnect" href="https://fonts.googleapis.com">
<link href="https://fonts.googleapis.com/css2?family=JetBrains+Mono:wght@400;700&display=swap" rel="stylesheet">
<style>
:root{color-scheme:dark}
body{margin:0;background:#000;color:#D7DADC;font:14px/20px 'JetBrains Mono',ui-monospace,monospace}
main{max-width:960px;margin:0 auto;padding:24px 16px 48px;display:flex;flex-direction:column;gap:22px}
a{color:#86BEFF}
.a{color:#F2A33A}.d{color:#A3AAB0}.ok{color:#6FD6B3}
.top{display:flex;justify-content:space-between;gap:16px;flex-wrap:wrap}
.tab{background:#F2A33A;color:#000;font-weight:700;padding:0 6px;margin-right:12px}
.p{position:relative;border:1px solid #545C63;padding:16px}
.pt{position:absolute;top:-10px;left:12px;background:#000;padding:0 6px;font-weight:700}
.kv{display:grid;grid-template-columns:max-content 1fr;gap:4px 24px}
.kv>:nth-child(odd){color:#A3AAB0}
code{color:#F4F5F6;overflow-wrap:anywhere}
.scroll{overflow-x:auto}
table{border-collapse:collapse;width:100%}
th{color:#A3AAB0;font-weight:400;text-align:left}
td,th{padding:4px 16px 4px 0;vertical-align:top}
.n{text-align:right;white-space:nowrap}
p{margin:0}
@media (max-width:640px){table,tbody,tr,td{display:block}tr:first-child{display:none}tr{padding:8px 0;border-top:1px solid #3A4045}td{padding:0;display:inline-block;white-space:nowrap;margin-right:12px}td:last-child{display:block;white-space:normal;margin:4px 0 0}}
</style>
</head>
<body>
<main>
<div class="top"><div><span class="tab">Feed</span>monokulo-nodo</div><div class="d">$host</div></div>
<div class="p"><div class="pt">What this is</div>
<p>An app feed for <a href="https://github.com/OceanEilonwy/monokulo-nodo">monokulo-nodo</a> devices: signed xbps packages, each checked with the device's own install review before this page was written. Add it by hand, comparing the key with the one below.</p>
</div>
<div class="p"><div class="pt">Add it</div>
<div class="kv">
<span>Address</span><code>$(printf %s "$address" | esc)</code>
<span>Key</span><code>$shown</code>
<span>Signed by</span><code>$signer</code>
<span>Public key</span><a href="signing-key.pub.pem">signing-key.pub.pem</a>
</div>
<p style="margin-top:12px" class="d">In the device's terminal: <span class="a">Feeds</span>, <span class="a">a</span> add, enter the address, and confirm the key only if it matches the one above.</p>
</div>
<div class="p"><div class="pt">Packages</div>
<div class="scroll"><table>
<tr><th>Name</th><th>Version</th><th>Arch</th><th class="n">Size</th><th>About</th></tr>
$rows</table></div>
</div>
<div class="d">Built $built from <a href="https://github.com/OceanEilonwy/monokulo-nodo/commit/$commit">$commit</a>. Each package was checked with the device's own install review before this page was written.</div>
</main>
</body>
</html>
EOF
	rm -f "$site"/.check-*.tsv
	echo "wrote $site: $(du -sh "$site" | cut -f1), largest file $(find "$site" -type f -printf '%s %f\n' | sort -n | tail -n 1)"
	;;
*) echo "unknown command: $cmd" >&2; exit 2 ;;
esac
