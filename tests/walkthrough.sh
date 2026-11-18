#!/bin/sh
# Runs scripts/walkthrough.sh against a stand-in device: ssh, curl, openssl, dig and getent are
# replaced by scripts that answer as a set-up T6N would, so nothing leaves this machine. Checks the
# report, that a secret the device prints reaches neither the report nor the log, the manual steps,
# verify, the release gate in each case, and that docs/RELEASE_WALKTHROUGH.md lists exactly the
# steps the script asks. Needs no device: the fast tier runs it in the checks container.
#   tests/walkthrough.sh
set -u
root=$(cd "$(dirname "$0")/.." && pwd)
wt=$root/scripts/walkthrough.sh
fail=0
result() { if [ "$2" = 0 ]; then echo "PASS $1"; else echo "FAIL $1"; fail=1; fi; }
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT
mkdir "$tmp/bin" "$tmp/out"

# What the recipes in this checkout build, which the stand-in device has installed.
sed -e 's/#.*//' -e '/^[[:space:]]*$/d' "$root/feed/base-packages" | while read -r p; do
	v=$(sed -n 's/^version=//p' "$root/packages/$p/template" | head -n 1)
	r=$(sed -n 's/^revision=//p' "$root/packages/$p/template" | head -n 1)
	echo "$p $p-${v}_$r"
done > "$tmp/installed"

cat > "$tmp/bin/ssh" <<EOF
#!/bin/sh
# The last argument is the remote command.
for last; do :; done
case \$last in
*walkthrough-ok*) echo walkthrough-ok; echo has-tui ;;
*device-tree/model*) printf 'FriendlyElec NanoPC-T6N\n6.18.62\ncapability,landlock,yama,bpf\n' ;;
*xbps-query*) cat "$tmp/installed" ;;
*'sv status'*) printf 'run: /var/service/nodo-helper: (pid 10) 500s; run: log: (pid 9) 500s\nrun: /var/service/cuprated: (pid 11) 500s\n' ;;
*'--ctl overview'*) printf 'Overview(\n    DeviceOverview {\n        hostname: "monokulo-nodo",\n        encryption: Unlocked,\n        ssh_keys: 1,\n    },\n)\n' ;;
*'--ctl health'*) echo 'Health(HealthSummary { ok: true })' ;;
*'--ctl system-status'*) printf 'SystemUpdate(\n    SystemUpdateStatus {\n        unavailable: None,\n        current_serial: 20261110041700,\n        current_snapshot: Some(\n            "2026.11.10-041700",\n        ),\n        last_check: Some(\n            1795000000,\n        ),\n        last_error: None,\n    },\n)\n' ;;
*'--ctl feeds'*) printf 'Feeds(\n    [\n        FeedInfo {\n            name: "monokulo",\n            address: "https://nodo.monokulo.com/feed",\n            key_changed: false,\n            problem: None,\n        },\n        FeedInfo {\n            name: "handicrafts",\n            address: "https://example.org/feed",\n            key_changed: false,\n            problem: None,\n        },\n    ],\n)\n    api_token: "supersecret-walkthrough-token",\n' ;;
*'--ctl services'*) printf 'Services(\n    [\n        ServiceStatus {\n            id: "handicrafts",\n            state: Running,\n        },\n    ],\n)\n' ;;
nodo-report) printf '== time\nnow\n== dns\nnameserver 192.0.2.1\nwifi passphrase: supersecret-walkthrough-token\n== kernel network and storage\nok\n' ;;
*boot/previous*) ;;
*) echo "unexpected remote command: \$last" >&2; exit 1 ;;
esac
EOF
cat > "$tmp/bin/curl" <<'EOF'
#!/bin/sh
printf 200
EOF
cat > "$tmp/bin/dig" <<'EOF'
#!/bin/sh
echo 192.0.2.80
EOF
cat > "$tmp/bin/getent" <<'EOF'
#!/bin/sh
case $2 in nodo.test) echo "192.0.2.10 STREAM nodo.test" ;; shop.example.com) echo "192.0.2.80 STREAM shop.example.com" ;; *) exit 2 ;; esac
EOF
cat > "$tmp/bin/openssl" <<'EOF'
#!/bin/sh
case $1 in
s_client) printf -- '-----BEGIN CERTIFICATE-----\nMIIB\n-----END CERTIFICATE-----\n' ;;
x509)
	case " $* " in
	*' -issuer '*) echo "issuer=C = US, O = Let's Encrypt, CN = ${STUB_ISSUER:-R11}" ;;
	*' -checkend '*) exit 0 ;;
	esac ;;
esac
EOF
chmod 755 "$tmp/bin/"*
PATH=$tmp/bin:$PATH
export PATH

out=$(sh "$wt" run nodo.test --site shop.example.com --local-site shop.local --out "$tmp/out" </dev/null 2>&1)
report=
for f in "$tmp/out"/*-release.txt; do [ -s "$f" ] && report=$f; done
log=${report%.txt}.log
if [ -n "$report" ]; then result "run writes a dated report" 0; else result "run writes a dated report" 1; printf '%s\n' "$out"; exit 1; fi
for id in A01 A03 A04 A05 A06 A07 A08 A09 A10 A11 A12 A15 A16 A17 A18; do
	grep -q "^step $id PASS " "$report" || { echo "$id did not pass:"; grep "^step $id " "$report"; printf '%s\n' "$out" | tail -n 5; false; } || break
done
result "every automated step passes on a healthy stand-in device" "$?"
grep -q '^step A02 TODO ' "$report" && grep -q '^step A19 TODO ' "$report" && grep -q '^step A13 SKIP ' "$report"
result "without a terminal or a run from outside, those steps stay open" "$?"
grep -qx 'pkgver nodo-tools-0.1.0_[0-9]*' "$report" && grep -qx 'snapshot 2026.11.10-041700' "$report" && grep -qx 'model FriendlyElec NanoPC-T6N' "$report"
result "the report records the versions, the snapshot and the board" "$?"
grep -qx 'result INCOMPLETE' "$report"
result "with manual steps open the result is INCOMPLETE" "$?"
! grep -q 'supersecret' "$report" "$log" && grep -q '== nodo-report' "$log"
result "a secret the device prints reaches neither the report nor the log" "$?"
! grep -qE 'nodo\.test|shop\.example\.com|192\.0\.2\.' "$report"
result "the report names no address or host" "$?"
! sh "$wt" verify "$report" >/dev/null
result "verify refuses an incomplete report" "$?"

# A version the checkout does not build fails A05.
sed 's/^nodo-tools nodo-tools-.*/nodo-tools nodo-tools-0.0.1_1/' "$tmp/installed" > "$tmp/installed.old"
cp "$tmp/installed" "$tmp/installed.keep"; cp "$tmp/installed.old" "$tmp/installed"
sh "$wt" run nodo.test --out "$tmp/out-old" </dev/null >/dev/null 2>&1
cp "$tmp/installed.keep" "$tmp/installed"
grep -q '^step A05 FAIL .*nodo-tools (device nodo-tools-0.0.1_1' "$tmp/out-old"/*-release.txt
result "a package at another version than the recipe fails A05" "$?"
# A staging certificate fails A17.
STUB_ISSUER="(STAGING) Ersatz Edamame E1" sh "$wt" run nodo.test --site shop.example.com --out "$tmp/out-staging" </dev/null >/dev/null 2>&1
grep -q '^step A17 FAIL ' "$tmp/out-staging"/*-release.txt
result "a Let's Encrypt staging certificate fails A17" "$?"

# The terminal step needs a real terminal, which a test does not have; mark it as one would.
sed 's/^step A02 TODO \([^:]*\):.*/step A02 PASS \1: local 80x24, over SSH 80x24/' "$report" > "$tmp/a02" && cat "$tmp/a02" > "$report"
sh "$wt" outside shop.example.com "$report" >/dev/null 2>&1
grep -q '^step A19 PASS ' "$report"
result "outside records A19" "$?"
# Answers in step order: p for required steps, s for optional ones, and one refused skip first.
answers=$(sh "$wt" steps | awk -F'|' '$1 ~ /^W/ { if ($2 == "req") print "p"; else print "s" }')
printf 's\n%s\n' "$answers" | sh "$wt" record "$report" > "$tmp/record.out" 2>&1
grep -q 'W01 is required; it cannot be skipped.' "$tmp/record.out"
result "record refuses to skip a required step" "$?"
grep -qx 'result PASS' "$report" && sh "$wt" verify "$report" >/dev/null
result "after the manual steps the report passes verify" "$?"
sed 's/^epoch .*/epoch 1000000000/' "$report" > "$tmp/stale.txt"
! sh "$wt" verify "$tmp/stale.txt" >/dev/null
result "verify refuses a report older than 30 days" "$?"
sed 's/^step W09 PASS \(.*\)/step W09 FAIL \1: the save was refused/' "$report" > "$tmp/failed.txt"
! sh "$wt" verify "$tmp/failed.txt" >/dev/null
result "verify refuses a report with a failed step" "$?"

# The gate. A snapshot's nodo repository holds its packages as files; the live manifest pins them.
mkdir -p "$tmp/repo" "$tmp/reports"
while read -r _ pv; do : > "$tmp/repo/$pv.aarch64.xbps"; done < "$tmp/installed"
{
	printf 'format = 1\nchannel = "stable"\nserial = 1\n\n[[repo]]\nname = "nodo"\n'
	while read -r name pv; do printf '\n[[packages]]\nname = "%s"\nversion = "%s"\n' "$name" "${pv#"$name"-}"; done < "$tmp/installed"
} > "$tmp/live.toml"
sh "$wt" gate "$tmp/repo" "$tmp/live.toml" "$tmp/reports" > "$tmp/gate.out" 2>&1
result "the gate passes a snapshot that changes none of this project's packages" "$?"
sed 's/^\(version = "0.1.0_\)[0-9]*"/\10"/' "$tmp/live.toml" > "$tmp/older.toml"
! sh "$wt" gate "$tmp/repo" "$tmp/older.toml" "$tmp/reports" > "$tmp/gate.out" 2>&1 &&
	grep -q 'no hardware walk-through covers this snapshot' "$tmp/gate.out"
result "the gate refuses changed packages without a walk-through" "$?"
cp "$tmp/failed.txt" "$tmp/reports/a-failed.txt"
! sh "$wt" gate "$tmp/repo" "$tmp/older.toml" "$tmp/reports" > "$tmp/gate.out" 2>&1
result "the gate refuses a walk-through that failed" "$?"
sed 's/^pkgver nodo-tools-.*/pkgver nodo-tools-0.0.1_1/' "$report" > "$tmp/reports/b-other.txt"
! sh "$wt" gate "$tmp/repo" "$tmp/older.toml" "$tmp/reports" > "$tmp/gate.out" 2>&1 &&
	grep -q 'did not run nodo-tools-' "$tmp/gate.out"
result "the gate refuses a walk-through of other versions" "$?"
cp "$report" "$tmp/reports/c-good.txt"
sh "$wt" gate "$tmp/repo" "$tmp/older.toml" "$tmp/reports" > "$tmp/gate.out" 2>&1 && grep -q 'covered by the walk-through' "$tmp/gate.out"
result "the gate passes with a walk-through of these versions" "$?"
rm "$tmp/reports/c-good.txt"
WALKTHROUGH_WAIVER="security fix, walked through after" GITHUB_STEP_SUMMARY=$tmp/summary \
	sh "$wt" gate "$tmp/repo" "$tmp/older.toml" "$tmp/reports" > "$tmp/gate.out" 2>&1 &&
	grep -q 'WITHOUT a hardware walk-through: security fix' "$tmp/gate.out" && grep -q 'Reason given: security fix' "$tmp/summary"
result "a waiver publishes and says so in the run summary" "$?"

# The document lists every manual step, with the same optional marks, and every automated one.
doc=$root/docs/RELEASE_WALKTHROUGH.md
sh "$wt" steps | awk -F'|' '$1 ~ /^[WP]/ { o = ($2 == "opt" || $3 == "opt") ? "opt" : "req"; print $1, o }' > "$tmp/script-steps"
sed -n 's/^### \([WP][0-9][0-9]\) · .*/\1/p' "$doc" | while read -r id; do
	if grep -q "^### $id · .*(optional)" "$doc"; then echo "$id opt"; else echo "$id req"; fi
done > "$tmp/doc-steps"
diff "$tmp/script-steps" "$tmp/doc-steps"
result "the document's manual steps match the script's, in order, with their optional marks" "$?"
missing=$(sh "$wt" steps | cut -d'|' -f1 | grep '^A' | while read -r id; do grep -q "^| $id |" "$doc" || echo "$id"; done)
[ -z "$missing" ] || echo "not in the document: $missing"
[ -z "$missing" ]
result "the document lists every automated step" "$?"
exit "$fail"
