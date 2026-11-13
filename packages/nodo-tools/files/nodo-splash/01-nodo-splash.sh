#!/bin/sh
# runit stage 1 (/etc/runit/core-services), sourced right after the pseudo-filesystems are
# mounted: the boot splash on the built-in screen (docs/SCREEN.md, "Boot splash"). Only boards
# whose screen shows the panels have nodo-gui (or nodo-screen, in text) enabled; anywhere else tty1
# keeps its text. Their run scripts stop the splash before they take the screen.
if [ -x /usr/bin/nodo-splash ] && [ -c /dev/tty1 ] \
	&& { [ -e /etc/runit/runsvdir/default/nodo-gui ] || [ -e /etc/runit/runsvdir/default/nodo-screen ]; }; then
	# Kernel messages would draw over the logo; they still reach the kernel log and the serial port.
	dmesg -n 1 2>/dev/null
	# Terminus 16x32 with Unicode block elements. The kernel's built-in Terminus also has the half
	# blocks the logo needs, so the splash still shows if this fails.
	setfont -C /dev/tty1 /usr/share/kbd/consolefonts/ter-v32n.psf.gz 2>/dev/null
	/usr/bin/nodo-splash --pidfile /run/nodo-splash.pid </dev/null >/dev/null 2>&1 &
fi
