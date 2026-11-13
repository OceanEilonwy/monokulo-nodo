#!/bin/sh
# runit stage 3 (/etc/runit/shutdown.d), sourced before 10-sv-stop.sh, whose `sv force-stop` kills
# any service still running after 7 seconds. A node saving its database during a sync needs longer,
# and a kill then corrupts it, so for an ordinary shutdown (poweroff or reboot over SSH, a critical
# temperature) nodo-power first stops cuprated and every package that declares a longer stop
# (stop_timeout_secs), waiting as long as each declares. The screen's Shut down button already did
# this; nodo-power then sees /run/nodo-power/off and returns at once.
if [ -x /usr/bin/nodo-power ] && [ ! -e /run/nodo-power/off ]; then
	echo "Waiting for services that save data to stop..."
	/usr/bin/nodo-power --before-shutdown </dev/null
fi
