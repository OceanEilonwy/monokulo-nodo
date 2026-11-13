#!/bin/sh
# Runs inside the disposable build container.
set -eu
trap 'chown -R "$HOST_UID:$HOST_GID" /src/build/hostdir /src/out' EXIT
vp=/work/void-packages
cp -r /src/packages/* "$vp/srcpkgs/"
cd "$vp"
echo "XBPS_MAKEJOBS=$(nproc)" >> etc/conf
export HOME=/work
# Compiler caches, kept in the hostdir next to the distfiles and bounded in size: by default 4 GB
# for sccache and 2 GB for ccache (a kernel build alone fills about 1.5 GB), less in CI, where
# .github/workflows/feed.yml sets SCCACHE_CACHE_SIZE and CCACHE_MAXSIZE to fit GitHub's 10 GB.
# C and C++: xbps-src's own ccache support puts /usr/lib/ccache/bin first in PATH, with
# CCACHE_DIR=$XBPS_HOSTDIR/ccache.
echo "XBPS_CCACHE=yes" >> etc/conf
mkdir -p /src/build/hostdir/ccache
CCACHE_DIR=/src/build/hostdir/ccache ccache --max-size "${CCACHE_MAXSIZE:-2G}" >/dev/null
# Rust: one sccache server for the whole run, started here so it has these settings whatever
# environment xbps-src gives the compiles. It never idles out, and goes with the container. cc-rs
# sees RUSTC_WRAPPER=sccache and sends the C and assembly of -sys crates through it too, so in Rust
# packages sccache, not ccache, caches those.
sccache=
if SCCACHE_DIR=/src/build/hostdir/sccache SCCACHE_CACHE_SIZE="${SCCACHE_CACHE_SIZE:-4G}" \
	SCCACHE_IDLE_TIMEOUT=0 sccache --start-server; then
	sccache=1
	for step in configure build check install; do
		cp /src/scripts/_xbps-src-sccache.sh "common/environment/$step/zz-monokulo-sccache.sh"
	done
else
	echo "warning: sccache did not start; Rust packages build without it" >&2
fi
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
	[ -z "$sccache" ] || sccache --zero-stats >/dev/null
	CCACHE_DIR=/src/build/hostdir/ccache ccache --zero-stats >/dev/null
	rc=0
	./xbps-src -H /src/build/hostdir $cross -f pkg "$pkg" || rc=$?
	# Hit rates for the log, also for a build that failed.
	echo "== compiler caches for $pkg"
	[ -z "$sccache" ] || sccache --show-stats || true
	CCACHE_DIR=/src/build/hostdir/ccache ccache --show-stats || true
	# The tree _xbps-src-sccache.sh moved out of $wrksrc, which xbps-src does not know to remove.
	rm -rf /work/void-packages/masterdir/builddir/.shared-src
	[ "$rc" = 0 ] || exit "$rc"
done
feed="/src/out/feed/$ARCH"
for pkg in $PKGS; do
	rm -f "$feed/$pkg"-[0-9]*.xbps
	# The hostdir keeps every revision ever built. Copy only the newest: the index below registers
	# files in name order with -f, so an older revision copied alongside (nodo-tools_8 sorts after
	# nodo-tools_11) would end up being the one images install.
	newest=$(ls /src/build/hostdir/binpkgs/"$pkg"-[0-9]*."$ARCH".xbps 2>/dev/null | sort -V | tail -n 1)
	[ -n "$newest" ] || { echo "package was not built: $pkg" >&2; exit 1; }
	cp "$newest" "$feed/"
done
XBPS_TARGET_ARCH="$ARCH" xbps-rindex -c "$feed" 2>/dev/null || true
XBPS_TARGET_ARCH="$ARCH" xbps-rindex -f -a "$feed"/*.xbps
