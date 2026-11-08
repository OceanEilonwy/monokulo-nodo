#!/bin/sh
# Usage: check-config.sh <fragment> <.config>
# Fails when olddefconfig dropped or changed anything the fragment asks for, which happens when
# a dependency is missing. Without this, a lost driver would only show up on the board.
set -eu
bad=0
while IFS= read -r line; do
	case $line in
	"# CONFIG_"*" is not set")
		sym=${line#\# }; sym=${sym%% *}
		if grep -qE "^$sym=" "$2"; then echo "wanted off: $(grep -E "^$sym=" "$2")"; bad=1; fi
		;;
	CONFIG_*=*)
		grep -qxF "$line" "$2" || { echo "wanted $line, got: $(grep -E "^(# )?${line%%=*}[= ]" "$2" || echo unset)"; bad=1; }
		;;
	esac
done < "$1"
exit "$bad"
