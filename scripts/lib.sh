# Shared by the host-side scripts.
root=$(cd "$(dirname "$0")/.." && pwd)
. "$root/versions.env"
tree_hash() { (cd "$root" && find "$@" -type f -print0 | sort -z | xargs -0 sha256sum | sha256sum | cut -c1-16); }
# A mirror change must not recompile anything, so only the recipe and build system count. A recipe
# built from this repository lists its source paths in a `workspace` file, and those count too.
pkg_hash() {
	src=
	# The list holds plain relative paths, one per line, so splitting it is intended.
	# shellcheck disable=SC2046
	[ -f "$root/packages/$1/workspace" ] && src=$(cd "$root" && tree_hash $(cat "packages/$1/workspace"))
	printf '%s %s %s' "$(tree_hash "packages/$1")" "$VOID_PACKAGES_REV" "$src" | sha256sum | cut -c1-16
}
builder_tag() { printf 'monokulo-builder:%s' "$(tree_hash docker/builder.Dockerfile versions.env)"; }
ensure_builder() {
	BUILDER=$(builder_tag)
	docker image inspect "$BUILDER" >/dev/null 2>&1 && return
	docker build -q -t "$BUILDER" --build-arg "VOID_IMAGE=$VOID_IMAGE" --build-arg "VOID_MIRROR=$VOID_MIRROR" \
		--build-arg "VOID_PACKAGES_REV=$VOID_PACKAGES_REV" -f "$root/docker/builder.Dockerfile" "$root/docker" >/dev/null
}
