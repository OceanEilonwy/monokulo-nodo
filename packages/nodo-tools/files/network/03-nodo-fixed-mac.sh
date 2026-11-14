#!/bin/sh
# runit stage 1 (/etc/runit/core-services), sourced after udev has loaded the network drivers and
# before any service (dhcpcd) starts: a fixed Ethernet address for boards whose card has none
# (nodo-fixed-mac explains). Runs in a subshell so its variables and `set -e` stay out of stage 1.
[ -x /usr/libexec/nodo/nodo-fixed-mac ] && ( /usr/libexec/nodo/nodo-fixed-mac ) || true
