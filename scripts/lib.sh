# Shared by the host-side scripts.
root=$(cd "$(dirname "$0")/.." && pwd)
. "$root/versions.env"
tree_hash() { (cd "$root" && find "$@" -type f -print0 | sort -z | xargs -0 sha256sum | sha256sum | cut -c1-16); }
# A mirror change must not recompile anything, so only the recipe and build system count.
# A recipe built from this repository names its source paths on a "# monokulo-sources:" line.
#
# NODO_EXTRA_PACKAGES may name a directory of third-party recipes laid out like void-packages'
# srcpkgs/ (a subpackage is a symlink to its recipe), such as Pingers' target/nodo/srcpkgs. Its
# recipes build exactly like those in packages/, which win on a clash.
if [ -n "${NODO_EXTRA_PACKAGES:-}" ]; then
	case $NODO_EXTRA_PACKAGES in
	/*) [ -d "$NODO_EXTRA_PACKAGES" ] || { echo "NODO_EXTRA_PACKAGES is not a directory: $NODO_EXTRA_PACKAGES" >&2; exit 2; } ;;
	*) echo "NODO_EXTRA_PACKAGES must be an absolute path: $NODO_EXTRA_PACKAGES" >&2; exit 2 ;;
	esac
fi
# The directory holding a package's recipe, through a subpackage's symlink.
recipe_dir() {
	if [ -e "$root/packages/$1/template" ]; then
		(cd -P "$root/packages/$1" && pwd)
	elif [ -n "${NODO_EXTRA_PACKAGES:-}" ] && [ -e "$NODO_EXTRA_PACKAGES/$1/template" ]; then
		(cd -P "$NODO_EXTRA_PACKAGES/$1" && pwd)
	else
		echo "no recipe for $1 in packages/${NODO_EXTRA_PACKAGES:+ or $NODO_EXTRA_PACKAGES}" >&2
		return 1
	fi
}
# Whether a name is a subpackage: a symlink to another recipe, built with it.
is_subpackage() {
	[ -L "$root/packages/$1" ] || { [ ! -e "$root/packages/$1" ] && [ -n "${NODO_EXTRA_PACKAGES:-}" ] && [ -L "$NODO_EXTRA_PACKAGES/$1" ]; }
}
pkg_hash() {
	if [ ! -e "$root/packages/$1/template" ]; then
		# An overlay recipe: its own files are all that count; its sources are pinned by checksum.
		dir=$(recipe_dir "$1") || return 1
		printf '%s %s' "$(cd "$dir" && find . -type f -print0 | sort -z | xargs -0 sha256sum | sha256sum | cut -c1-16)" \
			"$VOID_PACKAGES_REV" | sha256sum | cut -c1-16
		return
	fi
	extra=$(sed -n 's/^# monokulo-sources: //p' "$root/packages/$1/template")
	printf '%s %s' "$(tree_hash "packages/$1" $extra)" "$VOID_PACKAGES_REV" | sha256sum | cut -c1-16
}
builder_tag() { printf 'monokulo-builder:%s' "$(tree_hash docker/builder.Dockerfile versions.env)"; }
ensure_builder() {
	BUILDER=$(builder_tag)
	docker image inspect "$BUILDER" >/dev/null 2>&1 && return
	docker build -q -t "$BUILDER" --build-arg "VOID_IMAGE=$VOID_IMAGE" --build-arg "VOID_MIRROR=$VOID_MIRROR" \
		--build-arg "VOID_PACKAGES_REV=$VOID_PACKAGES_REV" -f "$root/docker/builder.Dockerfile" "$root/docker" >/dev/null
}
