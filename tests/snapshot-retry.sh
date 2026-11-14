#!/bin/sh
# publish-snapshot.sh index-and-test: copies Void's index again and retries only when the test
# step reports that Void published meanwhile (exit 75), at most SNAPSHOT_TEST_ATTEMPTS times;
# any other failure stops at once. The real index and test steps are replaced by a stand-in.
set -eu
here=$(cd "$(dirname "$0")/.." && pwd)
t=$(mktemp -d); trap 'rm -rf "$t"' EXIT
cat >"$t/fake" <<'FAKE'
#!/bin/sh
# Logs each call; the test step's exit codes come from $FAKE_CODES, one per call, in order.
echo "$1" >>"$FAKE_LOG"
[ "$1" = index ] && exit 0
n=$(grep -c '^test$' "$FAKE_LOG")
exit "$(echo "$FAKE_CODES" | cut -d' ' -f"$n")"
FAKE
chmod 755 "$t/fake"
run() { # codes... -> prints the call log and the exit code
	: >"$t/log"
	rc=0
	FAKE_LOG=$t/log FAKE_CODES="$*" PUBLISH_SNAPSHOT_SELF=$t/fake SNAPSHOT_ROOT=$t/out \
		sh "$here/scripts/publish-snapshot.sh" index-and-test snap board aarch64 2>/dev/null || rc=$?
	echo "$(tr '\n' ' ' <"$t/log")rc=$rc"
}
check() { [ "$2" = "$3" ] || { echo "FAIL: $1: got '$2', want '$3'"; exit 1; }; }
check "passes first time" "$(run 0)" "index test rc=0"
check "Void published once" "$(run 75 0)" "index test index test rc=0"
check "Void published every time" "$(run 75 75 75)" "index test index test index test rc=75"
check "another failure is not retried" "$(run 1 0)" "index test rc=1"
check "a later other failure stops" "$(run 75 2)" "index test index test rc=2"
echo "PASS snapshot test retries only when Void published meanwhile"
