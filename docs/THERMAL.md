# Heat on the Nodo

The Nodo has no fan. Its case is the heatsink, so the only way the board can shed heat faster
is to do less: the kernel slows the CPU clusters and the GPU when they get warm. Mainline's
RK3588 limits are written for boards with a fan or a large heatsink (slow down at 85 °C, power
off at 115 °C, hardware reset at 120 °C), and with those the Nodo ran hot for hours during its
first blockchain sync. The device tree (`packages/linux-mainline-rk3588/files/rk3588-nanopc-t6n.dts`)
replaces them with these:

| Temperature | Sensors | What happens |
|---|---|---|
| 70 °C | each A76 pair, the A55 cluster, the GPU | **warm**: that block may step down to a middle speed: A76 2.4 to 1.8 GHz, A55 1.8 to 1.416 GHz, GPU 1 GHz to 600 MHz |
| 80 °C | the same | **hot**: it may step on down to its lowest speed: A76 1.2 GHz, A55 1.008 GHz, GPU 300 MHz |
| 100 °C | all seven, including the SoC package, centre and NPU | **critical**: the kernel shuts down in order, so services such as cuprated stop and save their data |
| 110 °C | the TSADC itself | the chip resets in hardware, in case the kernel is stuck |

Both throttle steps have 3 °C of hysteresis: a block that slowed at 70 °C speeds up again once
it is back under 67 °C. The speeds are the cooling states of mainline's OPP tables
(`rk3588-opp.dtsi`): the A76 clusters have 7 (2.4 GHz down to 1.2 GHz), the A55 cluster 5
(1.8 GHz down to 1.008 GHz) and the GPU 8 (1 GHz down to 300 MHz). The warm step allows states 0
to 3, 0 to 2 and 0 to 4; the hot step allows the rest.

## Why these numbers

- **70 and 80 °C.** The RK3588 is specified to a junction temperature of 125 °C, but a passively
  cooled board that sits at 85 °C for days ages faster, and the case gets too hot to touch. A
  first sync keeps the CPUs busy for days, so the limits are set for that, not for bursts.
  Stepping down to 1.8 GHz first saves a lot for little speed: the top speeds need the highest
  voltages, and a core's power grows with the square of its voltage.
- **100 °C critical.** A critical trip makes the kernel run an orderly shutdown, which stops
  runit's services. The hardware reset does not: it cuts the chip off as a power cut would, and
  a power cut during a sync is what corrupted cuprated's database before. So the orderly
  shutdown comes well before the reset, and is reached only if throttling to the lowest speeds
  cannot hold the temperature, for example in direct sun.
  That shutdown waits for cuprated to save before runit's general stop, up to 300 seconds
  (`06-nodo-long-stop.sh`, docs/SCREEN.md, "Shutting down"); the kernel has no emergency
  poweroff delay that would cut it short, and the throttled chip stays far below the reset.
- **110 °C hardware reset.** Above the critical trip, so that it only acts when software cannot,
  and below Rockchip's own 120 °C.
- **step_wise, not power_allocator.** power_allocator needs the case's sustainable power in
  milliwatts, which nobody has measured, and runs a separate controller per zone (four here),
  each sharing out one budget. step_wise needs no tuning: above a trip it slows the block one
  speed at a time every 100 ms, within that trip's range, and speeds it back up once the
  temperature falls below the trip less its hysteresis. With two trips it is gentle at first
  and firm when that is not enough.

## The SSD

The Samsung 970 EVO Plus has its own sensor and slows its writes at about 82 °C. The kernel
reads it through `CONFIG_NVME_HWMON` (the `nvme` device in `/sys/class/hwmon`). The screen's
Device panel shows it as `ssd temp`, warns from 70 °C and shows an error from 80 °C; the panel
also warns when the chip reaches 75 °C, by which point the kernel is already slowing it.

## Checking

```sh
cat /sys/class/thermal/thermal_zone*/type /sys/class/thermal/thermal_zone*/temp
cat /sys/class/thermal/cooling_device*/cur_state      # 0 is full speed
cat /sys/devices/system/cpu/cpufreq/policy*/scaling_max_freq
for h in /sys/class/hwmon/hwmon*; do echo "$(cat $h/name) $(cat $h/temp1_input 2>/dev/null)"; done
```

`tests/inspect-t6n-mainline.sh` checks the trip temperatures, the hot steps' cooling maps and
the hardware reset temperature in a built image's device tree, and that `CONFIG_NVME_HWMON` is
on.
