#!/bin/sh
# The release walk-through on a real NanoPC-T6N (docs/RELEASE_WALKTHROUGH.md): the parts a laptop
# can check over SSH, the manual steps the tester answers, and the report a release needs.
#
#   scripts/walkthrough.sh run DEVICE [options]   check the device, write a dated report
#       --user USER          SSH user (default root, the only one the image lets in)
#       --app ID             the app installed during the walk-through (default handicrafts)
#       --site HOST          its public address, for the certificate and DNS checks
#       --local-site HOST    its home network address, such as handicrafts.local
#       --check-updates      also press "Check now" for the system and the apps (see below)
#       --manual             ask the manual steps as well, at the end
#       --after-release      the follow-up on the previous walk-through's card (steps P01 to P03)
#       --out DIR            where the report goes (default out/walkthrough)
#   scripts/walkthrough.sh outside HOST REPORT    from outside the home network: the public site
#   scripts/walkthrough.sh record REPORT          ask the manual steps REPORT still has open
#   scripts/walkthrough.sh verify REPORT          exit 0 only for a complete, passing report
#   scripts/walkthrough.sh gate NODO_REPO LIVE_MANIFEST [REPORT_DIR]
#                                                  CI: refuse a snapshot no walk-through covers
#   scripts/walkthrough.sh steps                   every step, for tests and the document
#
# Everything on the device is read-only: ssh runs only commands that report (xbps-query, sv
# status, nodo-tui --ctl with read requests, nodo-report). The one exception is --check-updates,
# which asks the helper to check for updates as "Check now" does; that records the time of the
# check on the device and nothing else. Nothing is installed, started, stopped or configured.
#
# Secrets: no step asks for the device passphrase or any other secret, and none is sent to the
# device. The helper's answers redact secrets by type, and everything kept from the device goes
# through redact() as well. The report (REPORT.txt) holds results, versions and the board, and no
# addresses or host names, so it can be committed or attached to a release; the log beside it
# (REPORT.log) has the full answers and nodo-report, for the tester only.
set -eu
root=$(cd "$(dirname "$0")/.." && pwd)

# id|in a release walk-through|in the after-release follow-up|what it checks
# req: must pass. opt: may be skipped, but a failure still fails the walk-through. -: not asked.
# A steps are automated here; W and P steps are manual, numbered as in docs/RELEASE_WALKTHROUGH.md.
steps() {
	cat <<'EOF'
A01|req|req|the laptop has ssh, curl and openssl
A02|req|req|terminal sizes of this terminal and of an SSH session to the device
A03|req|req|SSH logs in with a key, without a password, and runs a command without the TUI
A04|req|req|the board is a NanoPC-T6N on the mainline kernel, with Landlock
A05|req|req|this project's packages on the device are the versions of this checkout's recipes
A06|req|req|every enabled service is running
A07|req|req|the helper answers, and the device is set up and unlocked
A08|req|req|the health monitor answers
A09|req|req|system updates: a server is set, a check has run, and no error is recorded
A10|req|req|the default feed and at least one more are added, readable, with unchanged keys
A11|req|opt|the walk-through app is installed and running
A12|req|req|nodo-report runs to the end
A13|opt|opt|Check now: the system and app update checks answer
A14|opt|opt|a kernel update kept the previous kernel and device tree for putting back by hand
A15|req|opt|the public site has a DNS record at a public resolver
A16|req|opt|the device serves the public site over HTTPS with a certificate this laptop trusts
A17|req|opt|the public site's certificate is from Let's Encrypt's production service, 14 days or more left
A18|req|opt|the site answers over HTTPS on the home network
A19|req|opt|from outside the home network: DNS, HTTPS and the certificate of the public site
W01|req|-|boot the candidate card: splash, then the address on the screen, heartbeat LED
W02|req|-|first run over SSH at 80x24: passphrase rules, SSH keys, then Home
W03|req|-|keyboard at 80x24: tabs, help, Esc and scrolling, with nothing cut off
W04|req|-|a large terminal, and resizing while the TUI is open
W05|req|-|mouse: click tabs and rows, scroll, hover
W06|req|-|the project's feed lists its apps, read over the internet
W07|req|-|add a third-party feed and check its key
W08|req|-|install an app with a long settings form: progress, leave and come back, result
W09|req|-|fill, clear and save settings, with an empty optional number and a secret
W10|req|-|click fields on a scrolled settings form
W11|req|-|Check now with the network: it shows it is working, then a result
W12|req|-|Check now without the network: a plain error, then it recovers
W13|opt|-|apply an update that is offered, then undo it
W14|req|-|connect a domain provider that has several domains
W15|opt|-|a provider key that sees only some domains gets an honest message
W16|req|-|put the app on the home network and open it from another device
W17|req|-|put the app on the internet with a real certificate
W18|req|-|open the public site from outside the home network
W19|req|-|touchscreen: tiles, Manage, the screen PIN, Arrange, taps land where drawn
W20|req|-|reboot, unlock with the passphrase, apps and the site come back
W21|req|-|shut down from the touchscreen, unplug, power on, unlock
W22|req|-|back up to a USB stick
W23|req|-|restore that backup from the USB stick on this device
W24|opt|-|restore onto a freshly flashed card from first run
W25|req|-|Undo an update: what it offers is true, including the System section
W26|opt|-|kernel recovery drill: write the saved kernel back from another computer
W27|opt|-|take the site off the internet again
P01|-|req|on the previous walk-through's card, apply the new snapshot from System, Updates
P02|-|req|after the update: the TUI restarted itself, unlock again, apps still run
P03|-|opt|a snapshot that needs a restart: reboot, unlock, the new kernel runs
EOF
}

die() { echo "walkthrough: $*" >&2; exit 2; }
# Whether step $1 is asked in phase $2 (release or after-release), and how: req, opt or -.
need() {
	steps | awk -F'|' -v id="$1" -v phase="$2" '$1 == id { print (phase == "release" ? $2 : $3) }'
}
# Lines from the device that could hold a secret are left out of anything kept. The helper already
# prints secrets as Secret(<redacted>); this is a second guard, for output that is not the helper's.
redact() {
	awk '{ l = tolower($0)
		if (l ~ /token|secret|passw|passphrase|api_?key|private|psk|credential|bearer|authorization/)
			print "    <a line that may hold a secret was left out>"
		else print }'
}
# A report line's note: one line, no tabs, and nothing that looks like a secret.
clean() { printf '%s' "$*" | tr '\t\r\n' '   ' | redact | cut -c1-300; }

# Rewrites the result line from the step lines: FAIL if any step failed, INCOMPLETE if a required
# step has not passed, PASS otherwise.
recompute() {
	report=$1
	phase=$(sed -n 's/^phase //p' "$report")
	result=PASS
	for id in $(steps | cut -d'|' -f1); do
		mode=$(need "$id" "$phase")
		[ "$mode" = - ] && continue
		status=$(awk -v id="$id" '$1 == "step" && $2 == id { print $3 }' "$report")
		case $status in
		FAIL) result=FAIL ;;
		PASS) ;;
		*) [ "$mode" = req ] && [ "$result" = PASS ] && result=INCOMPLETE ;;
		esac
	done
	grep -v '^result ' "$report" > "$report.new"
	echo "result $result" >> "$report.new"
	mv "$report.new" "$report"
}

# Sets step $2 of report $1 to status $3 with note $4, keeping the steps in their order.
set_step() {
	text=$(steps | awk -F'|' -v id="$2" '$1 == id { print $4 }')
	[ -n "$text" ] || die "no step $2"
	note=$(clean "$4")
	line="step $2 $3 $text${note:+: $note}"
	awk -v id="$2" -v line="$line" '$1 == "step" && $2 == id { print line; next } { print }' "$1" > "$1.new"
	mv "$1.new" "$1"
	echo "$3 $2 $text${note:+: $note}"
}

# Asks every manual step of the report that is not PASS yet. Answers come from standard input.
ask_manual() {
	report=$1
	phase=$(sed -n 's/^phase //p' "$report")
	echo
	echo "Manual steps, as in docs/RELEASE_WALKTHROUGH.md. Answer p (passed), f (failed), s (skipped,"
	echo "optional steps only) or Enter to leave it open. Never type a passphrase or key here."
	for id in $(steps | cut -d'|' -f1 | grep -E '^[WP]'); do
		mode=$(need "$id" "$phase")
		[ "$mode" = - ] && continue
		status=$(awk -v id="$id" '$1 == "step" && $2 == id { print $3 }' "$report")
		[ "$status" = PASS ] && continue
		text=$(steps | awk -F'|' -v id="$id" '$1 == id { print $4 }')
		while :; do
			label=$id; [ "$mode" = opt ] && label="$id (optional)"
			printf '%s %s [p/f/s/Enter] ' "$label" "$text"
			read -r answer || answer=
			case $answer in
			p | P) set_step "$report" "$id" PASS "" >/dev/null; break ;;
			f | F)
				printf '  What went wrong, in a few words (no secrets): '
				read -r why || why=
				set_step "$report" "$id" FAIL "$why" >/dev/null
				break ;;
			s | S)
				if [ "$mode" = opt ]; then set_step "$report" "$id" SKIP "" >/dev/null; break; fi
				echo "  $id is required; it cannot be skipped." ;;
			'') break ;;
			*) echo "  p, f, s or Enter." ;;
			esac
		done
	done
	recompute "$report"
}

# Exit 0 when the report is a complete walk-through that passed and is recent enough.
verify() {
	report=$1
	[ -f "$report" ] || { echo "no such report: $report"; return 1; }
	grep -qx 'format 1' "$report" || { echo "$report: not a walk-through report (format 1)"; return 1; }
	phase=$(sed -n 's/^phase //p' "$report")
	case $phase in release | after-release) ;; *) echo "$report: unknown phase: $phase"; return 1 ;; esac
	ok=0
	for id in $(steps | cut -d'|' -f1); do
		mode=$(need "$id" "$phase")
		[ "$mode" = - ] && continue
		status=$(awk -v id="$id" '$1 == "step" && $2 == id { print $3 }' "$report")
		case $status in
		PASS) ;;
		FAIL) echo "$report: $id failed"; ok=1 ;;
		SKIP) [ "$mode" = opt ] || { echo "$report: $id is required and was skipped"; ok=1; } ;;
		*) [ "$mode" = opt ] || { echo "$report: $id has not passed (${status:-missing})"; ok=1; } ;;
		esac
	done
	epoch=$(sed -n 's/^epoch \([0-9][0-9]*\)$/\1/p' "$report")
	[ -n "$epoch" ] || { echo "$report: no epoch line"; return 1; }
	max=${WALKTHROUGH_MAX_AGE_DAYS:-30}
	age=$(( ($(date -u +%s) - epoch) / 86400 ))
	[ "$age" -le "$max" ] || { echo "$report: made $age days ago, more than $max"; ok=1; }
	return "$ok"
}

# The version of package $1 its recipe in this checkout builds, as xbps names it.
recipe_pkgver() {
	t=$root/packages/$1/template
	[ -f "$t" ] || return 1
	v=$(sed -n 's/^version=//p' "$t" | head -n 1)
	r=$(sed -n 's/^revision=//p' "$t" | head -n 1)
	[ -n "$v" ] && [ -n "$r" ] && echo "$1-${v}_$r"
}
base_packages() { sed -e 's/#.*//' -e '/^[[:space:]]*$/d' "$root/feed/base-packages"; }

# An address for curl --resolve and openssl -connect: IPv6 in brackets.
bracket() { case $1 in *:*) echo "[$1]" ;; *) echo "$1" ;; esac; }
# The first address of a name from this machine's resolver, or the argument if it is an address.
address_of() {
	case $1 in
	*[!0-9.]*) ;;
	*) echo "$1"; return ;;
	esac
	case $1 in *:*) echo "$1"; return ;; esac
	if command -v getent >/dev/null 2>&1; then
		getent ahosts "$1" 2>/dev/null | awk '{ print $1; exit }'
	else
		# macOS has no getent; ping prints the address it resolved.
		ping -c 1 "$1" 2>/dev/null | sed -n '1s/.*(\([0-9.]*\)).*/\1/p'
	fi
}
# Whether a public resolver has an A or AAAA record for $1. Falls back to this machine's resolver.
dns_check() {
	resolver=${WALKTHROUGH_RESOLVER:-1.1.1.1}
	if command -v dig >/dev/null 2>&1; then
		found=$(dig +short +time=5 +tries=2 "@$resolver" "$1" A "$1" AAAA 2>/dev/null | grep -cE '^[0-9a-fA-F:.]+$' || true)
		[ "$found" -gt 0 ] && { echo "$found records at $resolver"; return 0; }
		echo "no A or AAAA record at $resolver"
		return 1
	fi
	[ -n "$(address_of "$1")" ] && { echo "resolved by this machine (dig is not installed)"; return 0; }
	echo "this machine cannot resolve it"
	return 1
}
# Prints the issuer of the certificate served for name $1 at address $2 (port 443) and whether it
# is valid for 14 more days. Exit 0 only for Let's Encrypt production with 14 days or more left.
cert_check() {
	pem=$(openssl s_client -connect "$(bracket "$2"):443" -servername "$1" </dev/null 2>/dev/null |
		sed -n '/-----BEGIN CERTIFICATE-----/,/-----END CERTIFICATE-----/p')
	[ -n "$pem" ] || { echo "no certificate was served"; return 1; }
	issuer=$(printf '%s\n' "$pem" | openssl x509 -noout -issuer 2>/dev/null | sed 's/^issuer= *//')
	if printf '%s\n' "$pem" | openssl x509 -noout -checkend $((14 * 86400)) >/dev/null 2>&1; then left=yes; else left=no; fi
	echo "issuer $issuer; 14 days or more left: $left"
	case $issuer in *STAGING* | *Staging* | *Fake* | *Pebble*) return 1 ;; esac
	case $issuer in *"Let's Encrypt"*) ;; *) return 1 ;; esac
	[ "$left" = yes ]
}
# HTTPS status code of https://$1/, through address $2 if one is given, without checking the
# certificate if $3 is "insecure". 000 when the connection or the certificate check failed.
https_code() {
	set -- "$1" "${2:-}" "${3:-}"
	if [ "$3" = insecure ]; then k=-k; else k=--no-progress-meter; fi
	if [ -n "$2" ]; then
		curl -sS "$k" --max-time 20 -o /dev/null -w '%{http_code}' --resolve "$1:443:$(bracket "$2")" "https://$1/" 2>/dev/null || true
	else
		curl -sS "$k" --max-time 20 -o /dev/null -w '%{http_code}' "https://$1/" 2>/dev/null || true
	fi
}
http_ok() { case $1 in [1-4][0-9][0-9]) return 0 ;; *) return 1 ;; esac; }
valid_host() { case $1 in '' | *[!A-Za-z0-9.:-]* | -*) return 1 ;; *) return 0 ;; esac; }

cmd=${1:-}
[ $# -gt 0 ] && shift

case $cmd in
steps)
	steps
	;;
verify)
	[ $# = 1 ] || die "usage: walkthrough.sh verify REPORT"
	if verify "$1"; then echo "$1: a complete walk-through that passed"; else exit 1; fi
	;;
record)
	[ $# = 1 ] || die "usage: walkthrough.sh record REPORT"
	[ -f "$1" ] || die "no such report: $1"
	ask_manual "$1"
	tail -n 1 "$1"
	;;
outside)
	[ $# = 2 ] || die "usage: walkthrough.sh outside HOST REPORT"
	host=$1 report=$2
	valid_host "$host" || die "not a host name: $host"
	[ -f "$report" ] || die "no such report: $report"
	# Run from outside the home network (a phone's hotspot, say), so nothing here goes through the
	# home router's own view of the name: it is what a visitor gets.
	notes=$(dns_check "$host") && dns=0 || dns=1
	code=$(https_code "$host")
	addr=$(address_of "$host")
	cert=$( [ -n "$addr" ] && cert_check "$host" "$addr") && c=0 || c=1
	if [ "$dns" = 0 ] && http_ok "$code" && [ "$c" = 0 ]; then
		set_step "$report" A19 PASS "DNS: $notes; HTTPS $code; $cert"
	else
		set_step "$report" A19 FAIL "DNS: $notes; HTTPS ${code:-000}; ${cert:-no address}"
	fi
	recompute "$report"
	tail -n 1 "$report"
	;;
gate)
	# For .github/workflows/system.yml, before a snapshot is signed. A snapshot that only refreshes
	# Void's index (the weekly run) changes none of this project's packages, and passes. One that
	# changes any of them needs a release walk-through in REPORT_DIR that passed, is recent, and
	# ran on a device with every changed package at the snapshot's version. WALKTHROUGH_WAIVER, a
	# reason given by hand to a manual run, publishes without one and says so in the run summary.
	[ $# -ge 2 ] || die "usage: walkthrough.sh gate NODO_REPO LIVE_MANIFEST [REPORT_DIR]"
	repo=$1 live=$2 dir=${3:-$root/releases/walkthroughs}
	[ -d "$repo" ] || die "no repository directory: $repo"
	tmp=$(mktemp -d)
	trap 'rm -rf "$tmp"' EXIT
	for f in "$repo"/*.xbps; do
		[ -f "$f" ] || continue
		pv=${f##*/}
		echo "${pv%.*.xbps}"
	done | LC_ALL=C sort -u > "$tmp/snapshot"
	[ -s "$tmp/snapshot" ] || die "no packages in $repo"
	# The live manifest's [[packages]] pins, as name-version. None on the first publication.
	if [ -f "$live" ]; then
		awk '/^\[\[packages\]\]/ { inp = 1; name = ""; next }
			/^\[/ { inp = 0 }
			inp && /^name *=/ { name = $0; sub(/^name *= *"/, "", name); sub(/".*/, "", name) }
			inp && /^version *=/ { v = $0; sub(/^version *= *"/, "", v); sub(/".*/, "", v); if (name != "") print name "-" v }' \
			"$live" | LC_ALL=C sort -u > "$tmp/live"
	else
		: > "$tmp/live"
	fi
	LC_ALL=C comm -23 "$tmp/snapshot" "$tmp/live" > "$tmp/changed"
	if [ ! -s "$tmp/changed" ]; then
		echo "this snapshot changes none of this project's packages; no walk-through needed"
		exit 0
	fi
	echo "this snapshot changes: $(tr '\n' ' ' < "$tmp/changed")"
	if [ -n "${WALKTHROUGH_WAIVER:-}" ]; then
		reason=$(clean "$WALKTHROUGH_WAIVER")
		echo "::warning::published WITHOUT a hardware walk-through: $reason"
		if [ -n "${GITHUB_STEP_SUMMARY:-}" ]; then
			{
				echo "### Published without a hardware walk-through"
				echo
				echo "Reason given: $reason"
				echo
				echo "Changed packages: $(tr '\n' ' ' < "$tmp/changed")"
			} >> "$GITHUB_STEP_SUMMARY"
		fi
		exit 0
	fi
	for report in "$dir"/*.txt; do
		[ -f "$report" ] || continue
		[ "$(sed -n 's/^phase //p' "$report")" = release ] || continue
		why=$(verify "$report") || { echo "not usable: $why"; continue; }
		missing=$(while read -r pv; do grep -qx "pkgver $pv" "$report" || echo "$pv"; done < "$tmp/changed")
		if [ -z "$missing" ]; then
			echo "covered by the walk-through in $report"
			exit 0
		fi
		echo "not usable: $report did not run $(echo "$missing" | tr '\n' ' ')"
	done
	# The workflow holds a pushed snapshot back without failing, so it asks for a notice there;
	# a manual run, which is meant to publish, keeps the error.
	echo "::${WALKTHROUGH_GATE_LEVEL:-error}::no hardware walk-through covers this snapshot. Walk through docs/RELEASE_WALKTHROUGH.md" \
		"on a card built from this commit, commit its report to releases/walkthroughs/, then run this workflow" \
		"again by hand."
	exit 1
	;;
run)
	device='' user=root app=handicrafts site='' local_site='' check_updates='' manual='' phase=release out=$root/out/walkthrough
	while [ $# -gt 0 ]; do
		case $1 in
		--user) user=${2:?}; shift ;;
		--app) app=${2:?}; shift ;;
		--site) site=${2:?}; shift ;;
		--local-site) local_site=${2:?}; shift ;;
		--check-updates) check_updates=1 ;;
		--manual) manual=1 ;;
		--after-release) phase=after-release ;;
		--out) out=${2:?}; shift ;;
		-*) die "unknown option: $1" ;;
		*) [ -z "$device" ] || die "one device only"; device=$1 ;;
		esac
		shift
	done
	[ -n "$device" ] || die "usage: walkthrough.sh run DEVICE [options]"
	valid_host "$device" || die "not a host name or address: $device"
	case $user in '' | *[!a-z0-9_-]*) die "bad user: $user" ;; esac
	case $app in '' | *[!a-z0-9_.-]*) die "bad app id: $app" ;; esac
	[ -z "$site" ] || valid_host "$site" || die "not a host name: $site"
	[ -z "$local_site" ] || valid_host "$local_site" || die "not a host name: $local_site"
	mkdir -p "$out"
	stamp=$(date -u +%Y%m%d-%H%M%S)
	report=$out/$stamp-$phase.txt
	log=$out/$stamp-$phase.log
	umask 077
	: > "$log"
	keep() { { echo; echo "== $1"; redact; } >> "$log"; }
	# -n: never read this script's standard input, which the manual steps use. BatchMode: a key or
	# nothing, never a password prompt. The device's host key is checked as usual.
	dev() { ssh -n -o BatchMode=yes -o ConnectTimeout=15 "$user@$device" "$@"; }

	commit=$(git -C "$root" rev-parse --short=12 HEAD 2>/dev/null || echo unknown)
	[ -z "$(git -C "$root" status --porcelain 2>/dev/null)" ] || commit="$commit (with uncommitted changes)"
	{
		echo "# monokulo-nodo release walk-through, docs/RELEASE_WALKTHROUGH.md"
		echo "format 1"
		echo "phase $phase"
		echo "date $(date -u +%Y-%m-%dT%H:%M:%SZ)"
		echo "epoch $(date -u +%s)"
		echo "commit $commit"
		echo "app $app"
		for id in $(steps | cut -d'|' -f1); do
			[ "$(need "$id" "$phase")" = - ] && continue
			echo "step $id TODO $(steps | awk -F'|' -v id="$id" '$1 == id { print $4 }')"
		done
	} > "$report"
	# Header lines learned below go just before the first step.
	header() {
		awk -v line="$1" '!done && $1 == "step" { print line; done = 1 } { print }' "$report" > "$report.new"
		mv "$report.new" "$report"
	}

	echo "Walk-through report: $report"
	missing=
	for tool in ssh curl openssl; do command -v "$tool" >/dev/null 2>&1 || missing="$missing $tool"; done
	if [ -z "$missing" ]; then
		extra=; command -v dig >/dev/null 2>&1 || extra="; no dig, so DNS is checked with this machine's resolver"
		set_step "$report" A01 PASS "ssh, curl, openssl$extra"
	else
		set_step "$report" A01 FAIL "missing:$missing"
	fi

	# The sizes the TUI gets: this terminal's, and the pseudo-terminal an SSH login gets from it.
	min_w=$(sed -n 's/^pub const MIN_WIDTH: u16 = \([0-9]*\);/\1/p' "$root/crates/nodo-tui/src/app.rs")
	min_h=$(sed -n 's/^pub const MIN_HEIGHT: u16 = \([0-9]*\);/\1/p' "$root/crates/nodo-tui/src/app.rs")
	if [ -t 0 ] && local_size=$(stty size </dev/tty 2>/dev/null); then
		lw=${local_size#* } lh=${local_size% *}
		header "terminal local ${lw}x$lh"
		remote_size=$(ssh -tt -o BatchMode=yes -o ConnectTimeout=15 "$user@$device" 'stty size' </dev/tty 2>/dev/null | tr -d '\r' | tail -n 1) || remote_size=
		rw=${remote_size#* } rh=${remote_size% *}
		case "$lw$lh$rw$rh" in '' | *[!0-9]*) remote_size= ;; esac
		if [ -n "$remote_size" ]; then
			header "terminal ssh ${rw}x$rh"
			if [ "$rw" -ge "${min_w:-60}" ] && [ "$rh" -ge "${min_h:-16}" ] && [ "$rw" = "$lw" ] && [ "$rh" = "$lh" ]; then
				set_step "$report" A02 PASS "local ${lw}x$lh, over SSH ${rw}x$rh, the TUI needs ${min_w:-60}x${min_h:-16}"
			else
				set_step "$report" A02 FAIL "local ${lw}x$lh, over SSH ${rw}x$rh, the TUI needs ${min_w:-60}x${min_h:-16}"
			fi
		else
			set_step "$report" A02 FAIL "local ${lw}x$lh; an SSH session reported no size"
		fi
	else
		set_step "$report" A02 TODO "not run from a terminal; run it in the terminal the walk-through used"
	fi

	if hello=$(dev 'echo walkthrough-ok; [ -t 0 ] && echo tty; command -v nodo-tui >/dev/null && echo has-tui' 2>&1); then
		keep "ssh" <<EOF
$hello
EOF
		case $hello in
		*tty*) set_step "$report" A03 FAIL "the command got a terminal; ssh -n should not give one" ;;
		*walkthrough-ok*has-tui*) set_step "$report" A03 PASS "key login, a command runs without the TUI" ;;
		*) set_step "$report" A03 FAIL "logged in, but nodo-tui is not installed" ;;
		esac
	else
		set_step "$report" A03 FAIL "ssh failed: $(printf '%s' "$hello" | tail -n 1)"
		echo "Without SSH nothing else can be checked; the report is $report"
		recompute "$report"
		[ -z "$manual" ] || ask_manual "$report"
		tail -n 1 "$report"
		exit 1
	fi

	board=$(dev 'tr -d "\0" < /proc/device-tree/model 2>/dev/null; echo; uname -r; cat /sys/kernel/security/lsm 2>/dev/null; echo' 2>/dev/null || true)
	model=$(printf '%s\n' "$board" | sed -n 1p)
	kernel=$(printf '%s\n' "$board" | sed -n 2p)
	lsm=$(printf '%s\n' "$board" | sed -n 3p)
	header "model $(clean "$model")"
	header "kernel $(clean "$kernel")"
	case $model in
	*NanoPC-T6*)
		case ",$lsm," in
		*,landlock,*) set_step "$report" A04 PASS "$model, kernel $kernel, Landlock on" ;;
		*) set_step "$report" A04 FAIL "$model, kernel $kernel, no Landlock (the release image is the mainline card)" ;;
		esac ;;
	*) set_step "$report" A04 FAIL "the board says ${model:-nothing}" ;;
	esac

	# Names come from this checkout's feed/base-packages, so they are plain package names.
	names=$(base_packages | tr '\n' ' ')
	case $names in *[!a-z0-9.\ _-]*) die "unexpected characters in feed/base-packages" ;; esac
	installed=$(dev "for p in $names; do v=\$(xbps-query -p pkgver \"\$p\" 2>/dev/null); echo \"\$p \${v:-absent}\"; done" 2>/dev/null || true)
	differ=
	for p in $names; do
		got=$(printf '%s\n' "$installed" | awk -v p="$p" '$1 == p { print $2 }')
		want=$(recipe_pkgver "$p" || echo "no-recipe")
		case $got in
		absent | '') header "pkgver $p absent" ;;
		*) header "pkgver $(clean "$got")" ;;
		esac
		[ "$got" = "$want" ] || differ="$differ $p (device ${got:-unknown}, recipe $want)"
	done
	if [ -z "$differ" ]; then
		set_step "$report" A05 PASS "$(echo "$names" | wc -w | tr -d ' ') packages match"
	else
		set_step "$report" A05 FAIL "differ:$differ"
	fi

	services=$(dev 'sv status /var/service/* 2>&1' 2>/dev/null || true)
	keep "services" <<EOF
$services
EOF
	notrun=$(printf '%s\n' "$services" | grep -v '^run:' | sed -n 's/^[a-z]*: *\/var\/service\/\([^:]*\):.*/\1/p' | tr '\n' ' ')
	if [ -n "$services" ] && [ -z "$notrun" ] && ! printf '%s\n' "$services" | grep -qv '^run:'; then
		set_step "$report" A06 PASS "$(printf '%s\n' "$services" | grep -c '^run:') services running"
	else
		set_step "$report" A06 FAIL "not running: ${notrun:-could not read sv status}"
	fi

	overview=$(dev 'nodo-tui --ctl overview' 2>&1) && ov=0 || ov=1
	keep "overview" <<EOF
$overview
EOF
	keys=$(printf '%s\n' "$overview" | sed -n 's/^ *ssh_keys: \([0-9]*\),$/\1/p')
	if [ "$ov" = 0 ] && printf '%s\n' "$overview" | grep -q 'encryption: Unlocked'; then
		if [ "${keys:-0}" -ge 1 ]; then
			set_step "$report" A07 PASS "unlocked, $keys SSH key(s)"
		else
			set_step "$report" A07 FAIL "unlocked, but no SSH key is listed"
		fi
	else
		state=$(printf '%s\n' "$overview" | sed -n 's/^ *encryption: \([A-Za-z]*\),$/\1/p')
		set_step "$report" A07 FAIL "encryption ${state:-unknown}; unlock it from the TUI first"
	fi

	health=$(dev 'nodo-tui --ctl health' 2>&1) && h=0 || h=1
	keep "health" <<EOF
$health
EOF
	if [ "$h" = 0 ]; then set_step "$report" A08 PASS ""; else set_step "$report" A08 FAIL "$(printf '%s' "$health" | tail -n 1)"; fi

	if [ -n "$check_updates" ]; then
		# The one request that writes: it records when the device last checked, as Check now does.
		sc=$(dev 'nodo-tui --ctl system-check' 2>&1) && s1=0 || s1=1
		uc=$(dev 'nodo-tui --ctl check-updates' 2>&1) && s2=0 || s2=1
		keep "system-check" <<EOF
$sc
EOF
		keep "check-updates" <<EOF
$uc
EOF
		if [ "$s1" = 0 ] && [ "$s2" = 0 ]; then
			set_step "$report" A13 PASS "both answered"
		else
			set_step "$report" A13 FAIL "system check exit $s1, app check exit $s2"
		fi
	else
		set_step "$report" A13 SKIP "not asked (--check-updates)"
	fi

	status=$(dev 'nodo-tui --ctl system-status' 2>&1) && st=0 || st=1
	keep "system-status" <<EOF
$status
EOF
	# nodo-tui --ctl prints Rust's pretty Debug form, where Some("name") spans three lines.
	snapshot=$(printf '%s\n' "$status" | awk '/^ *current_snapshot: Some\($/ { getline; gsub(/[ ",]/, ""); print; exit }')
	[ -n "$snapshot" ] || snapshot="none, the image's own"
	header "snapshot $snapshot"
	problems=
	[ "$st" = 0 ] || problems="the helper did not answer"
	printf '%s\n' "$status" | grep -q '^ *unavailable: None,' || problems="$problems; updates are unavailable"
	printf '%s\n' "$status" | grep -q '^ *last_error: None,' || problems="$problems; the last check or update failed (see the log)"
	printf '%s\n' "$status" | grep -q '^ *last_check: None,' && problems="$problems; no check has run yet (press Check now, or wait ten minutes)"
	if [ -z "$problems" ]; then
		set_step "$report" A09 PASS "installed snapshot $snapshot"
	else
		set_step "$report" A09 FAIL "${problems#; }"
	fi

	feeds=$(dev 'nodo-tui --ctl feeds' 2>&1) && fd=0 || fd=1
	keep "feeds" <<EOF
$feeds
EOF
	count=$(printf '%s\n' "$feeds" | grep -c '^ *address: "' || true)
	if [ "$fd" != 0 ]; then
		set_step "$report" A10 FAIL "the helper did not list feeds"
	elif printf '%s\n' "$feeds" | grep -q '^ *problem: Some('; then
		set_step "$report" A10 FAIL "a feed reports a problem (see the log)"
	elif printf '%s\n' "$feeds" | grep -q '^ *key_changed: true'; then
		set_step "$report" A10 FAIL "a feed's key changed"
	elif [ "$count" -lt 2 ]; then
		set_step "$report" A10 FAIL "$count feed(s); add the third-party feed (W07)"
	else
		set_step "$report" A10 PASS "$count feeds"
	fi

	svc=$(dev 'nodo-tui --ctl services' 2>&1) || svc=
	keep "services (helper)" <<EOF
$svc
EOF
	state=$(printf '%s\n' "$svc" | awk -v id="id: \"$app\"," '$0 ~ "^ *" id "$" { f = 1; next } f && /^ *state: / { s = $2; sub(/,$/, "", s); print s; exit }')
	case $state in
	Running) set_step "$report" A11 PASS "$app is running" ;;
	'') set_step "$report" A11 FAIL "$app is not installed" ;;
	*) set_step "$report" A11 FAIL "$app is $state" ;;
	esac

	nr=$(dev nodo-report 2>&1 || true)
	keep "nodo-report" <<EOF
$nr
EOF
	if printf '%s\n' "$nr" | grep -qx '== kernel network and storage'; then
		set_step "$report" A12 PASS "kept in the log beside the report"
	else
		set_step "$report" A12 FAIL "it stopped early (see the log)"
	fi

	# /var/lib/nodo/boot is root only; this only lists it.
	prev=$(dev 'ls /var/lib/nodo/boot/previous 2>/dev/null' 2>/dev/null || true)
	case $prev in
	*kernel.part*resource.part* | *resource.part*kernel.part*) set_step "$report" A14 PASS "kernel.part and resource.part are kept" ;;
	'') set_step "$report" A14 SKIP "no kernel update on this card yet" ;;
	*) set_step "$report" A14 FAIL "only: $(echo "$prev" | tr '\n' ' ')" ;;
	esac

	if [ -n "$site" ]; then
		devaddr=$(address_of "$device")
		if notes=$(dns_check "$site"); then set_step "$report" A15 PASS "$notes"; else set_step "$report" A15 FAIL "$notes"; fi
		# Through the device's own address, so this works inside the home network whether or not the
		# router loops traffic for its public address back in. The certificate is still checked
		# against this laptop's trusted roots for the public name. A19 is the visitor's view.
		code=$(https_code "$site" "$devaddr")
		if http_ok "$code"; then
			set_step "$report" A16 PASS "HTTP $code"
		else
			set_step "$report" A16 FAIL "HTTP ${code:-000} (000: no connection, or a certificate this laptop does not trust)"
		fi
		if cert=$(cert_check "$site" "$devaddr"); then set_step "$report" A17 PASS "$cert"; else set_step "$report" A17 FAIL "$cert"; fi
	fi
	if [ -n "$local_site" ]; then
		# A .local name may not resolve on this laptop (no mDNS); the device's address stands in.
		via=
		[ -n "$(address_of "$local_site")" ] || via=$(address_of "$device")
		# -k: a .local site is signed by the device's own CA, which this laptop need not trust.
		code=$(https_code "$local_site" "$via" insecure)
		how=; [ -n "$via" ] && how=", through the device's address (the name does not resolve here)"
		if http_ok "$code"; then
			set_step "$report" A18 PASS "HTTP $code$how"
		else
			set_step "$report" A18 FAIL "HTTP ${code:-000}$how"
		fi
	fi

	recompute "$report"
	[ -z "$manual" ] || ask_manual "$report"
	echo
	echo "Report: $report"
	echo "Log, for you only: $log"
	[ -n "$site" ] && grep -q '^step A19 TODO' "$report" &&
		echo "From outside the home network, run: scripts/walkthrough.sh outside $site $report"
	grep -q '^step [WP][0-9]* TODO' "$report" && echo "Manual steps still open: scripts/walkthrough.sh record $report"
	tail -n 1 "$report"
	;;
*)
	sed -n '2,/^set -eu/p' "$0" | sed -e '/^set -eu/d' -e 's/^# \{0,1\}//' >&2
	exit 2
	;;
esac
