#!/bin/sh
# /etc/profile.d/nodo-tui.sh: opens nodo-tui when someone logs in over SSH (docs/ARCHITECTURE.md,
# "Logging in over SSH"). Sourced by login shells only, so `ssh nodo some-command`, scp and sftp
# never see it. Quitting nodo-tui leaves the normal shell; `nodo-tui` opens it again.
# To skip it: `touch ~/.hushtui`, or log in with `ssh -t nodo sh` (not a login shell).
nodo_tui_open() {
	case $- in *i*) ;; *) return 0 ;; esac
	[ -n "${SSH_TTY:-}" ] || return 0
	[ -z "${NODO_NO_TUI:-}" ] || return 0
	[ -z "${NODO_TUI_SHOWN:-}" ] || return 0
	[ ! -e "${HOME:-/}/.hushtui" ] || return 0
	[ -t 0 ] && [ -t 1 ] || return 0
	case ${TERM:-dumb} in dumb | unknown | "") return 0 ;; esac
	command -v nodo-tui >/dev/null 2>&1 || return 0
	# Exported so that a shell started from this one (`sh -l`, su) does not open it again.
	NODO_TUI_SHOWN=1
	export NODO_TUI_SHOWN
	nodo-tui || echo "nodo-tui stopped (exit $?)."
	echo "This is the Nodo's shell. Type nodo-tui to open the Nodo's screens again."
}
nodo_tui_open
unset -f nodo_tui_open
