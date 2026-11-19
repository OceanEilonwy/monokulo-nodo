#!/bin/sh
# Builds an app's packages from a release tag of its own repository. Apps keep their recipes in
# their own repositories, under nodo/, and publish their own feed: their release workflow calls
# .github/workflows/app-release.yml, which runs these steps in a checkout of this repository, then
# signs the packages into a feed with the app's own key and attaches it to the tag's GitHub
# release, so https://github.com/<owner>/<repo>/releases/latest/download is the app's feed.
# docs/FEED.md, "App feeds", describes the whole flow.
#
#   scripts/app-release.sh pin <app-checkout> <owner/repo> <tag> <package...>
#       copy <app-checkout>/nodo/<package> to packages/<package>, pinned to the tag: version= from
#       the tag, _commit= its commit and the source tarball's checksum; and copy the app's tests in
#       <app-checkout>/nodo/tests/<crate>/*.rs into crates/<crate>/tests
#   scripts/app-release.sh newer <owner/repo> <tag> <package...>
#       refuse a pinned version that is not newer than every one the repository's other releases
#       carry, which devices would refuse as a downgrade (or never install, when equal)
#   scripts/app-release.sh test
#       run the tests pin copied in, against this repository's crates
#   scripts/app-release.sh check <package...> -- <arch...>
#       sign out/feed/<arch>'s packages into a throwaway feed with a throwaway key and run every
#       package through the device's own install review (publish-feed.sh check)
#
# A recipe here is an ordinary feed app recipe whose first distfile is
# https://github.com/<owner>/<repo>/archive/${_commit}.tar.gz and whose first checksum is that
# tarball's. In the app's repository it stays buildable as it is (pinned to whatever commit it
# names); pin rewrites only those lines. The revision is kept when the tag's version is the
# recipe's own, and is 1 otherwise.
set -eu
. "$(dirname "$0")/lib.sh"
cmd=${1:?usage: app-release.sh pin|newer|test|check ...}
shift
state=$root/out/app-release
plain() { case $1 in *[!A-Za-z0-9._-]* | .* | -* | '') echo "bad $2: $1" >&2; exit 2 ;; esac; }
field() { sed -n "s/^$2=\"\{0,1\}\([^\"]*\)\"\{0,1\}\$/\1/p" "$1" | head -n 1; }

case $cmd in
pin)
	app=${1:?the app checkout}
	repo=${2:?owner/repo}
	tag=${3:?the release tag}
	shift 3
	[ $# -gt 0 ] || { echo "name the packages to pin" >&2; exit 2; }
	case $repo in */*/* | /* | */ | *[!A-Za-z0-9._/-]*) echo "bad repository: $repo" >&2; exit 2 ;; esac
	commit=$(git -C "$app" rev-parse "$tag^{commit}")
	# xbps versions hold no '-' or '_': v1.2.0-rc1 becomes 1.2.0rc1, which xbps sorts below 1.2.0.
	version=$(printf %s "${tag#v}" | tr -d -)
	case $version in [0-9]*) ;; *) echo "the tag $tag does not start with a version" >&2; exit 2 ;; esac
	case $version in *[!A-Za-z0-9.]*) echo "the tag $tag gives the version $version, which xbps does not take" >&2; exit 2 ;; esac
	mkdir -p "$state"
	tarball=$(mktemp)
	trap 'rm -f "$tarball"' EXIT
	curl -fsSL --retry 3 -o "$tarball" "https://github.com/$repo/archive/$commit.tar.gz"
	sum=$(sha256sum "$tarball" | cut -d' ' -f1)
	: > "$state/packages"
	for pkg in "$@"; do
		plain "$pkg" package
		src=$app/nodo/$pkg
		[ -f "$src/template" ] || { echo "no recipe at $src/template" >&2; exit 1; }
		[ "$(field "$src/template" pkgname)" = "$pkg" ] || { echo "$src/template is not pkgname=$pkg" >&2; exit 1; }
		# shellcheck disable=SC2016 # the recipe's own ${_commit}, literally
		grep -q "^distfiles=\"https://github.com/$repo/archive/\${_commit}.tar.gz" "$src/template" ||
			{ echo "$src/template: its first distfile must be https://github.com/$repo/archive/\${_commit}.tar.gz" >&2; exit 1; }
		grep -q '^_commit=[0-9a-f]\{40\}$' "$src/template" || { echo "$src/template has no _commit= line" >&2; exit 1; }
		grep -q '^checksum="[0-9a-f]\{64\}' "$src/template" || { echo "$src/template: checksum= must start with a sha256" >&2; exit 1; }
		was=$(field "$src/template" version)
		revision=1
		[ "$was" != "$version" ] || revision=$(field "$src/template" revision)
		rm -rf "$root/packages/$pkg"
		cp -R "$src" "$root/packages/$pkg"
		sed -i -e "s/^version=.*/version=$version/" -e "s/^revision=.*/revision=$revision/" \
			-e "s/^_commit=.*/_commit=$commit/" -e "s/^checksum=\"[0-9a-f]\{64\}/checksum=\"$sum/" \
			"$root/packages/$pkg/template"
		echo "$pkg" >> "$state/packages"
		echo "pinned $pkg-${version}_$revision to $repo@$commit (source sha256 $sum)"
	done
	: > "$state/tests"
	for dir in "$app"/nodo/tests/*/; do
		[ -d "$dir" ] || continue
		crate=$(basename "$dir")
		[ -d "$root/crates/$crate/tests" ] || { echo "nodo/tests/$crate: this repository has no crates/$crate" >&2; exit 1; }
		for t in "$dir"*.rs; do
			[ -f "$t" ] || continue
			to=$root/crates/$crate/tests/$(basename "$t")
			git -C "$root" ls-files --error-unmatch "$to" >/dev/null 2>&1 &&
				{ echo "nodo/tests/$crate/$(basename "$t") would replace this repository's own test" >&2; exit 1; }
			cp "$t" "$to"
			printf '%s %s\n' "$crate" "$(basename "$t" .rs)" >> "$state/tests"
		done
	done
	echo "copied $(wc -l < "$state/tests") test files"
	;;
newer)
	repo=${1:?owner/repo}
	tag=${2:?the release tag}
	shift 2
	[ $# -gt 0 ] || { echo "name the packages" >&2; exit 2; }
	# Every package file the app's other releases carry: the feed a device added is the latest
	# of them, and any of them may be what it installed.
	assets=$(gh api --paginate "repos/$repo/releases" \
		-q ".[] | select(.draft | not) | select(.tag_name != \"$tag\") | .assets[].name")
	fail=0
	for pkg in "$@"; do
		plain "$pkg" package
		t=$root/packages/$pkg/template
		new=$(field "$t" version)_$(field "$t" revision)
		# <pkg>-<version>_<revision>.<arch>.xbps; a version starts with a digit, so mk-pay never
		# matches mk-pay-something.
		olds=$(printf '%s\n' "$assets" | sed -n "s/^$pkg-\([0-9][^/]*_[0-9]*\)\.[a-z0-9_]*\.xbps\$/\1/p" | sort -u)
		if [ -z "$olds" ]; then
			echo "$pkg-$new: no earlier release of $repo carries it"
			continue
		fi
		# The newest by xbps' own ordering, then compared with this one.
		r=0
		old=$(docker run --rm --network none -e "NEW=$new" -e "OLDS=$olds" "$VOID_IMAGE" sh -euc '
			max=
			for v in $OLDS; do
				if [ -z "$max" ]; then max=$v; continue; fi
				c=0; xbps-uhelper cmpver "$v" "$max" || c=$?
				[ "$c" != 1 ] || max=$v
			done
			echo "$max"
			xbps-uhelper cmpver "$NEW" "$max"') || r=$?
		if [ "$r" = 1 ]; then
			echo "$pkg-$new: newer than $repo's earlier $pkg-$old"
		else
			echo "$pkg-$new is not newer than $repo's earlier $pkg-$old: devices would never install it." \
				"Tag a higher version, or raise revision= in nodo/$pkg/template for the same version." >&2
			fail=1
		fi
	done
	exit "$fail"
	;;
test)
	[ -f "$state/tests" ] || { echo "run app-release.sh pin first" >&2; exit 1; }
	for crate in $(cut -d' ' -f1 "$state/tests" | sort -u); do
		set --
		for t in $(sed -n "s/^$crate //p" "$state/tests"); do set -- "$@" --test "$t"; done
		(cd "$root" && cargo test --locked -p "$crate" "$@")
	done
	;;
check)
	pkgs=
	while [ $# -gt 0 ] && [ "$1" != -- ]; do plain "$1" package; pkgs="$pkgs $1"; shift; done
	[ "${1:-}" = -- ] || { echo "usage: app-release.sh check <package...> -- <arch...>" >&2; exit 2; }
	shift
	[ -n "$pkgs" ] && [ $# -gt 0 ] || { echo "usage: app-release.sh check <package...> -- <arch...>" >&2; exit 2; }
	work=$state/check
	rm -rf "$work"
	mkdir -p "$work"
	printf '%s\n' $pkgs > "$work/packages"
	openssl genrsa -out "$work/key.pem" 4096 2>/dev/null
	openssl rsa -in "$work/key.pem" -pubout -out "$work/key.pub.pem" 2>/dev/null
	ssh-keygen -i -m PKCS8 -f "$work/key.pub.pem" > "$work/key.ssh"
	fingerprint=$(ssh-keygen -l -E md5 -f "$work/key.ssh" | sed -n 's/^[0-9]* MD5:\([0-9a-f:]*\) .*/\1/p')
	[ -n "$fingerprint" ] || { echo "could not read the throwaway key's fingerprint" >&2; exit 1; }
	export FEED_PACKAGES="$work/packages" FEED_SITE="$work/site" FEED_FINGERPRINT="$fingerprint" FEED_SIGNER="app release check"
	"$root/scripts/publish-feed.sh" sign "$work/key.pem" "$@"
	"$root/scripts/publish-feed.sh" check "$@"
	rm -f "$work/key.pem"
	;;
*) echo "unknown command: $cmd" >&2; exit 2 ;;
esac
