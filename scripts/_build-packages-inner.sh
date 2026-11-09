#!/bin/sh
# Runs inside the disposable build container.
set -eu
trap 'chown -R "$HOST_UID:$HOST_GID" /src/build/hostdir /src/out' EXIT
vp=/work/void-packages
cp -r /src/packages/* "$vp/srcpkgs/"
cd "$vp"
echo "XBPS_MAKEJOBS=$(nproc)" >> etc/conf
# xbps-src builds a dependency from the pinned srcpkgs when the mirror's version differs (dhcpcd,
# for monokulo-base, since the mirror moved to 10.5), which needs patch and make; the
# builder image lacks both.
command -v make >/dev/null && command -v patch >/dev/null || xbps-install -y make patch >/dev/null
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
