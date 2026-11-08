#!/bin/sh
# Runs inside the disposable build container.
set -eu
trap 'chown -R "$HOST_UID:$HOST_GID" /src/build/hostdir /src/out' EXIT
vp=/work/void-packages
cp -r /src/packages/* "$vp/srcpkgs/"
# Recipes built from this repository get its source as a tarball, since xbps-src fetches nothing.
for pkg in $PKGS; do
	list=/src/packages/$pkg/workspace
	[ -f "$list" ] || continue
	mkdir -p "$vp/srcpkgs/$pkg/files"
	(cd /src && xargs tar -cf "$vp/srcpkgs/$pkg/files/workspace.tar" < "$list")
done
cd "$vp"
echo "XBPS_MAKEJOBS=$(nproc)" >> etc/conf
export HOME=/work
# xbps-src cross-compiles only when the target differs from this machine.
cross=
[ "$ARCH" = "$(xbps-uhelper arch)" ] || cross="-a $ARCH"
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
