#!/bin/sh
# runit stage 1 (/etc/runit/core-services), sourced right after the pseudo-filesystems are
# mounted: the boot splash on the built-in screen (docs/SCREEN.md, "Boot splash"). Only boards
# whose screen shows the panels have nodo-gui (or nodo-screen, in text) enabled; anywhere else tty1
# keeps its text. Their run scripts stop the splash before they take the screen.
if [ -x /usr/bin/nodo-splash ] && [ -c /dev/tty1 ] \
	&& { [ -e /etc/runit/runsvdir/default/nodo-gui ] || [ -e /etc/runit/runsvdir/default/nodo-screen ]; }; then
	# Kernel messages would draw over the logo; they still reach the kernel log and the serial port.
	dmesg -n 1 2>/dev/null
	# The panel's console only appears once udev has loaded the display driver, a few seconds
	# later; nodo-splash loads its font and redraws centred when the console changes size.
	/usr/bin/nodo-splash --pidfile /run/nodo-splash.pid </dev/null >/dev/null 2>&1 &
fi
