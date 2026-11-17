#!/bin/sh
# Refuses a change to a package's sources that leaves its version and revision as they were.
# Usage: check-revisions.sh <base-ref>   (compares <base-ref>...HEAD)
#
# The builds rebuild a package whenever its recipe hash changes (pkg_hash in lib.sh: its
# packages/<name> directory plus the paths on its "# monokulo-sources:" line). xbps only
# compares versions, though: a rebuilt package with the old version and revision is never
# installed on a device that already has that version, so the device silently keeps the old
# build. nodo-tools went from PR #29 to #35 that way and devices kept a TUI that could not
# read bundle manifests.
set -eu
root=$(cd "$(dirname "$0")/.." && pwd)
cd "$root"
base=${1:?usage: check-revisions.sh <base-ref>}
fail=0
for template in packages/*/template; do
	dir=$(dirname "$template")
	# A subpackage is a symlink to its recipe, which this loop checks.
	[ -L "$dir" ] && continue
	# A recipe the base does not have is new, and has no installed version to fall behind.
	git cat-file -e "$base:$template" 2>/dev/null || continue
	extra=$(sed -n 's/^# monokulo-sources: //p' "$template")
	# shellcheck disable=SC2086 # extra is a list of paths
	git diff --quiet "$base...HEAD" -- "$dir" $extra && continue
	old=$(git show "$base:$template" | grep -E '^(version|revision)=' || true)
	new=$(grep -E '^(version|revision)=' "$template" || true)
	if [ "$old" = "$new" ]; then
		echo "$(basename "$dir"): its sources changed but $(echo "$new" | tr '\n' ' ')did not;" \
			"raise revision= in $template, or devices that have this version never get the change" >&2
		fail=1
	fi
done
exit "$fail"
