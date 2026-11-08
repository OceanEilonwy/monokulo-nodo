#!/bin/sh
# Builds only packages whose recipe changed. The feed in out/feed/<arch> persists.
# Usage: scripts/build-packages.sh [arch] [package...]
set -eu
. "$(dirname "$0")/lib.sh"
only_list=
if [ "${1:-}" = --changed ]; then only_list=1; shift; fi
arch=${1:-aarch64}
[ $# -gt 0 ] && shift
[ $# -gt 0 ] || set -- cuprated cuprated-service monokulo-base nodo-caddy nodo-ddns
[ -n "$only_list" ] || ensure_builder
feed=$root/out/feed/$arch
mkdir -p "$root/build/hostdir" "$feed" "$root/build/stamps"
todo=
for pkg in "$@"; do
	want=$(pkg_hash "$pkg")
	if [ "$(cat "$root/build/stamps/$pkg.$arch" 2>/dev/null)" = "$want" ] && ls "$feed/$pkg"-[0-9]*.xbps >/dev/null 2>&1; then
		[ -n "$only_list" ] || echo "up to date: $pkg"
	else
		todo="$todo $pkg"
		eval "stamp_$(printf %s "$pkg" | tr -c 'a-zA-Z0-9' _)=$want"
	fi
done
[ -n "$todo" ] || exit 0
if [ -n "$only_list" ]; then echo $todo; exit 0; fi
docker run --rm -e "ARCH=$arch" -e "PKGS=$todo" -e "HOST_UID=$(id -u)" -e "HOST_GID=$(id -g)" \
	-v "$root:/src" "$BUILDER" /src/scripts/_build-packages-inner.sh
# Record the recipe hash from before the build, so edits made during it trigger a rebuild.
for pkg in $todo; do
	eval "printf '%s\n' \"\$stamp_$(printf %s "$pkg" | tr -c 'a-zA-Z0-9' _)\"" > "$root/build/stamps/$pkg.$arch"
done
