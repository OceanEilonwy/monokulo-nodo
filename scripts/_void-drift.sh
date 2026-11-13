#!/bin/sh
# Runs inside the build container. Images and the dev VM install Void's packages from the live
# mirror, while xbps-src builds from the void-packages recipes pinned at VOID_PACKAGES_REV. This
# prints one line for each package whose version differs between the two:
#   <package> <version in the mirror or root> <version of the pinned recipe>
#   _void-drift.sh repo <arch> <package>...   these packages in the mirror for <arch>
#   _void-drift.sh root <dir>                 every Void package installed in a root filesystem
# A package with no pinned recipe (this project's own, or one Void removed) is not listed. The
# status is 0 when nothing differs and 3 when something does.
set -eu
vp=/work/void-packages
. /src/versions.env
drift=$(mktemp)
trap 'rm -f "$drift"' EXIT
# xbps-checkvers -s prints: pkgname version-in-repo-or-root version-of-recipe srcpkg repository.
# "?" stands for a version it did not find.
keep() { awk '$2 != "?" && $3 != "?" && $2 != $3 { print $1, $2, $3 }' >> "$drift"; }
case ${1:-} in
repo)
	arch=${2:?usage: _void-drift.sh repo <arch> <package>...}
	shift 2
	[ $# -gt 0 ] || exit 0
	# Only <arch>'s mirror: the caller passes a cross build's host tools with the host's
	# architecture and its target packages with the target's.
	case $arch in aarch64) url=$VOID_MIRROR/current/aarch64 ;; *) url=$VOID_MIRROR/current ;; esac
	XBPS_TARGET_ARCH=$arch xbps-install -S -i -R "$url" >/dev/null
	XBPS_TARGET_ARCH=$arch xbps-checkvers -i -R "$url" -D "$vp" -sm "$@" | keep
	;;
root)
	dir=${2:?usage: _void-drift.sh root <dir>}
	xbps-checkvers -I -r "$dir" -D "$vp" -s | keep
	;;
*) echo "usage: _void-drift.sh repo <arch> <package>... | root <dir>" >&2; exit 2 ;;
esac
sort -u "$drift"
[ ! -s "$drift" ] || exit 3
