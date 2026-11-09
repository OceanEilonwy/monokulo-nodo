# monokulo-nodo: set the system clock from the board's battery-backed RTC (the HYM8563, rtc0).
# The kernel does this at boot when the RTC driver is ready in time; this covers the case where
# it was not. A clock that was never set fails here and chrony sets the time from the network.
if [ -z "$IS_CONTAINER" ] && [ -e /dev/rtc0 ]; then
    hwclock --hctosys --utc --noadjfile 2>/dev/null || msg_warn "the RTC has no valid time"
fi
