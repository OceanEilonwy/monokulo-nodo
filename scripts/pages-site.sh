#!/bin/sh
# The GitHub Pages site at https://nodo.monokulo.com has two halves, published by two workflows:
# the app feed (.github/workflows/feed.yml: index.html, CNAME, signing-key.pub.pem, feed/) and the
# system snapshots (.github/workflows/system.yml: snapshots/). A Pages deployment replaces the
# whole site, so each workflow writes its own half and copies the other half from the live site.
# Each half lists its files and their SHA-256 in files/<half>.txt, which is how the other workflow
# knows what to copy and checks that it copied it whole.
#
#   scripts/pages-site.sh list <site> <half> <path>...  write <site>/files/<half>.txt for these paths
#   scripts/pages-site.sh pull <site> <half>            copy <half> from the live site into <site>
#
# The live site is $PAGES_URL (default https://nodo.monokulo.com). A half the live site does not
# list yet (before its workflow first ran) is left out with a warning; any other failure stops.
# Nothing here is trusted by devices: the feed's index is signed, and so is every snapshot manifest.
set -eu
cmd=${1:?usage: pages-site.sh list|pull <site> <half> ...}
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
