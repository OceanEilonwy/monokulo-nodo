#!/bin/sh
# runit stage 3 (/etc/runit/shutdown.d), sourced before the services are stopped: the splash says
# "shutting down…" while they stop, the filesystems are unmounted and the power goes off. A shutdown
# that draws its own last screen (the screen's Shut down button) creates /run/nodo-splash.skip
# first, and is left alone.
if [ -x /usr/bin/nodo-splash ] && [ -c /dev/tty1 ] && [ ! -e /run/nodo-splash.skip ] \
	&& { [ -e /etc/runit/runsvdir/default/nodo-gui ] || [ -e /etc/runit/runsvdir/default/nodo-screen ]; }; then
	# The panels give up the screen first (nodo-gui hands the display back to tty1); they leave on
	# SIGTERM.
	sv -w 5 stop nodo-gui nodo-screen >/dev/null 2>&1
	/usr/bin/nodo-splash --stopping </dev/null >/dev/null 2>&1 &
fi
