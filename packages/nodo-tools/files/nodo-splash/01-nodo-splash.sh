#!/bin/sh
# runit stage 1 (/etc/runit/core-services), sourced right after the pseudo-filesystems are
# mounted: the boot splash on the built-in screen (docs/SCREEN.md, "Boot splash"). Only boards
# whose screen shows the panels have nodo-screen enabled; anywhere else tty1 keeps its text.
# nodo-screen's run script stops the splash before it takes tty1.
if [ -x /usr/bin/nodo-splash ] && [ -e /etc/runit/runsvdir/default/nodo-screen ] && [ -c /dev/tty1 ]; then
	# Kernel messages would draw over the logo; they still reach the kernel log and the serial port.
	dmesg -n 1 2>/dev/null
	# Terminus 16x32 with Unicode block elements. The kernel's built-in Terminus also has the half
	# blocks the logo needs, so the splash still shows if this fails.
	setfont -C /dev/tty1 /usr/share/kbd/consolefonts/ter-v32n.psf.gz 2>/dev/null
	/usr/bin/nodo-splash --pidfile /run/nodo-splash.pid </dev/null >/dev/null 2>&1 &
fi
