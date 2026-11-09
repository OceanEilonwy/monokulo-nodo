#!/bin/sh
# Runs inside the disposable build container.
set -eu
trap 'chown -R "$HOST_UID:$HOST_GID" /src/build/hostdir /src/out' EXIT
vp=/work/void-packages
cp -r /src/packages/* "$vp/srcpkgs/"
cd "$vp"
echo "XBPS_MAKEJOBS=$(nproc)" >> etc/conf
export HOME=/work
# xbps-src cross-compiles only when the target differs from this machine.
cross=
[ "$ARCH" = "$(xbps-uhelper arch)" ] || cross="-a $ARCH"
own=$(ls /src/packages)
# The hostdir is a local repository that xbps-src prefers to the mirror. A Void package in it can
# only be a dependency xbps-src once compiled from an older recipe, which would hide the mirror's
# version from every later build, so only this project's packages stay.
hostrepo=/src/build/hostdir/binpkgs
stray=
for f in "$hostrepo"/*.xbps; do
	[ -e "$f" ] || continue
	pkgver=$(basename "$f" .xbps)
	printf '%s\n' "$own" | grep -qxF "$(xbps-uhelper getpkgname "${pkgver%.*}")" && continue
	echo "removing ${pkgver%.*} from the hostdir: it is not one of this project's packages"
	rm -f "$f"
	stray=1
done
if [ -n "$stray" ]; then
	for index in "$hostrepo"/*-repodata; do
		[ -e "$index" ] || continue
		a=$(basename "$index" -repodata)
		XBPS_TARGET_ARCH=$a xbps-rindex -c "$hostrepo" >/dev/null
	done
fi
# xbps-src takes a dependency from the mirror only at the pinned recipe's exact version, and
# otherwise compiles the old recipe from source, for a package images then install at the
# mirror's version anyway. So a Void dependency that moved on in the mirror stops the build here.
deps=$(for pkg in $PKGS; do ./xbps-src $cross show-build-deps "$pkg"; done | grep -vxF "$own" | sort -u)
[ -z "$cross" ] || deps="$deps cross-$(./xbps-src -a "$ARCH" show-var XBPS_CROSS_TRIPLET)"
rc=0
drift=$(/src/scripts/_void-drift.sh repo "$ARCH" $deps) || rc=$?
if [ "$rc" != 0 ]; then
	[ "$rc" = 3 ] || exit "$rc"
	echo "Void's mirror and the void-packages recipes at VOID_PACKAGES_REV disagree on:" >&2
	printf '%s\n' "$drift" | awk '{ printf "  %s: mirror %s, recipe %s\n", $1, $2, $3 }' >&2
	echo "Set VOID_PACKAGES_REV in versions.env to a void-packages commit with the mirror's versions." >&2
	exit 1
fi
for pkg in $PKGS; do
	./xbps-src -H /src/build/hostdir $cross -f pkg "$pkg"
done
feed="/src/out/feed/$ARCH"
for pkg in $PKGS; do
	rm -f "$feed/$pkg"-[0-9]*.xbps
	cp /src/build/hostdir/binpkgs/"$pkg"-[0-9]*."$ARCH".xbps "$feed/" || { echo "package was not built: $pkg" >&2; exit 1; }
done
XBPS_TARGET_ARCH="$ARCH" xbps-rindex -c "$feed" 2>/dev/null || true
XBPS_TARGET_ARCH="$ARCH" xbps-rindex -f -a "$feed"/*.xbps
