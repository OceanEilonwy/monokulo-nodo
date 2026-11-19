#!/bin/sh
# The GitHub Pages site at https://nodo.monokulo.com: the system snapshots (snapshots/) and a
# landing page, all published by .github/workflows/system.yml. It once had a second half, the app
# feed (feed/), published by another workflow; apps now publish their own feeds from their own
# repositories (docs/FEED.md). A Pages deployment replaces the whole site, so each run copies the
# published snapshots from the live site, by the list of files and SHA-256s in files/<half>.txt.
#
#   scripts/pages-site.sh home <site>                   write the landing page, 404.html and CNAME
#   scripts/pages-site.sh list <site> <half> <path>...  write <site>/files/<half>.txt for these paths
#   scripts/pages-site.sh pull <site> <half>            copy <half> from the live site into <site>
#
# The live site is $PAGES_URL (default https://nodo.monokulo.com). A half the live site does not
# list yet (before its workflow first ran) is left out with a warning; any other failure stops.
# Nothing here is trusted by devices: every snapshot manifest is signed.
set -eu
cmd=${1:?usage: pages-site.sh home|list|pull <site> ...}
if [ "$cmd" = home ]; then
	site=${2:?the site directory}
	mkdir -p "$site"
	printf '%s\n' nodo.monokulo.com > "$site/CNAME"
	# GitHub Pages answered a missing file inside a directory with HTTP 503 while the site had no
	# 404 page (2026-11-13). This page is meant to make that a 404. Devices do not rely on it.
	printf %s\\n "<!doctype html><title>Not found</title><p>Not found." > "$site/404.html"
	cat > "$site/index.html" <<'EOF'
<!doctype html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>monokulo-nodo</title>
<meta name="description" content="System updates for monokulo-nodo devices.">
<style>
:root{color-scheme:dark}
body{margin:0;background:#000;color:#D7DADC;font:14px/20px ui-monospace,monospace}
main{max-width:720px;margin:0 auto;padding:24px 16px 48px;display:flex;flex-direction:column;gap:16px}
a{color:#86BEFF}
.tab{background:#F2A33A;color:#000;font-weight:700;padding:0 6px;margin-right:12px}
p{margin:0}
</style>
</head>
<body>
<main>
<div><span class="tab">nodo</span>monokulo-nodo</div>
<p>This site serves the signed system snapshots monokulo-nodo devices update from, under <code>snapshots/</code>. There is nothing to open here by hand.</p>
<p>Apps are not published here: each app publishes its own signed feed from its own repository, which you add on your device under Sources. See <a href="https://github.com/OceanEilonwy/monokulo-nodo/blob/main/docs/FEED.md">docs/FEED.md</a>.</p>
</main>
</body>
</html>
EOF
	echo "wrote the landing page in $site"
	exit 0
fi
site=${2:?the site directory}
half=${3:?feed or system}
case $half in feed | system) ;; *) echo "unknown half: $half" >&2; exit 2 ;; esac
shift 3
mkdir -p "$site/files"
site=$(cd "$site" && pwd)
url=${PAGES_URL:-https://nodo.monokulo.com}

case $cmd in
list)
	[ $# -gt 0 ] || { echo "name the paths of the $half half" >&2; exit 2; }
	for p in "$@"; do
		case $p in /* | *..*) echo "paths are relative to the site: $p" >&2; exit 2 ;; esac
		[ -e "$site/$p" ] || { echo "no such path in the site: $p" >&2; exit 1; }
	done
	(cd "$site" && find "$@" -type f | LC_ALL=C sort | while read -r f; do sha256sum "$f"; done) > "$site/files/$half.txt.new"
	mv "$site/files/$half.txt.new" "$site/files/$half.txt"
	echo "the $half half: $(wc -l < "$site/files/$half.txt") files"
	;;
pull)
	list=$site/files/$half.txt
	code=$(curl -sS --retry 3 -o "$list.new" -w '%{http_code}' "$url/files/$half.txt")
	if [ "$code" = 404 ]; then
		rm -f "$list.new"
		echo "::warning::the live site lists no $half half yet; publishing without it"
		exit 0
	fi
	[ "$code" = 200 ] || { echo "could not read $url/files/$half.txt: HTTP $code" >&2; exit 1; }
	mv "$list.new" "$list"
	n=0
	while read -r sum path; do
		case $path in /* | *..* | '') echo "refusing the path $path" >&2; exit 1 ;; esac
		mkdir -p "$site/$(dirname "$path")"
		curl -fsS --retry 3 -o "$site/$path" "$url/$path"
		echo "$sum  $site/$path" | sha256sum -c --quiet - || { echo "$path changed while copying; run again" >&2; exit 1; }
		n=$((n + 1))
	done < "$list"
	echo "copied the live $half half: $n files"
	;;
*) echo "unknown command: $cmd" >&2; exit 2 ;;
esac
