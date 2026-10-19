# Hardware setup

## What you need

- **K+DCAN USB cable** (FTDI FT232RL or CH340 chip). macOS has drivers for both
  built in, so there's nothing to install.
- **20-pin round to 16-pin OBD-II adapter**, for a Euro E36. Euro cars from this
  era (including a 1997 M52B25) don't have the OBD-II socket under the dash.
  The only diagnostic socket is the round 20-pin connector under the bonnet,
  near the strut tower. These adapters are cheap.
- A **battery charger or maintainer**. It isn't needed for reading, but it is
  required for any future flashing.

## Cable switch

Most K+DCAN cables have a switch or button with two positions: one bridges
OBD pins 7+8 (for older cars) and the other is for D-CAN (2007+). For the E36,
**use the "pins 7+8 bridged" position**. On the 20-pin adapter, the engine
ECU's data lines end up on OBD pins 7 and 8, and bridging them puts both on the
cable's K-line.

## Connecting on the Mac

1. Plug the cable into the Mac (through a USB-C adapter if needed), then into
   the 20-pin adapter, then into the car.
2. Turn the ignition to **position II** (dash lights on, engine off).
3. Run `e36 ports`. The cable shows up as something like
   `/dev/cu.usbserial-A50285BI` (FTDI) or `/dev/cu.wchusbserial1410` (CH340).
   macOS lists every port twice, so always use the `/dev/cu.*` name rather than
   `/dev/tty.*`.
4. Run `e36 ident --trace`.

## If it doesn't respond

| Symptom | Try |
|---|---|
| `no USB serial cable found` | Different USB port or adapter. Check System Information → USB to see if the cable shows up at all. |
| `no response from ECU` | Ignition really in position II? Cable switch in the 7+8 position? Adapter fully seated in the 20-pin socket and the screw collar tightened? |
| `echo mismatch` or garbage | Run again with `--echo off`, then with `--dtr low` or `--dtr high`. Some cables use DTR to select K-line mode. |
| Still stuck | Run with `--trace` and save the output. Those bytes tell us exactly what's happening. |

## Logging with the engine running

A running engine keeps the ECU busier and puts electrical noise on the line.
The tools deal with this themselves:

- **They pace their requests.** The pause between requests adjusts itself:
  it grows when the ECU answers "busy" or doesn't answer, and shrinks again
  when replies come back cleanly. Expect roughly 6–13 readings a second
  depending on how busy the ECU is.
- **Errors are retried, not fatal.** A corrupted reply is thrown away, the
  line is left to go quiet, and the next request goes out.
- **The app reconnects on its own** if the link really drops (cable pulled,
  or several seconds without a good reply), and a running recording carries
  on once it's back.

The **link %** next to the read rate in the app shows the share of good
replies; hover over it for details. If it stays low (below about 80%) with
the engine running, the line is noisier than it should be. Try:

- running the Mac on battery rather than from an in-car inverter or charger,
  which can create a ground loop
- plugging the cable straight into the Mac, not through a USB hub
- a clip-on ferrite on the USB cable near the K+DCAN end
- checking that the 20-pin adapter is fully seated with its collar tightened

`e36 live --trace` prints every byte, including noise that was thrown away,
which shows exactly what the line is doing.

## Adding a wideband O2 sensor

A wideband kit is a sensor, a controller and usually a gauge. Fitted like this,
it **doesn't touch the ECU at all**: the ECU never sees it, and it won't cause
fault codes.

- **Fit it in its own bung.** Have a bung welded into the exhaust after the
  manifold collector and before the catalytic converter. Leave the stock O2
  sensors in place, because the ECU needs them for its own fuelling. Mount it on
  the top half of the pipe, between the 10 and 2 o'clock positions, so the tip
  points down and condensation drains away from it.
- **Don't replace the stock O2 sensor with the wideband** unless the controller
  has a "simulated narrowband output" wired to the ECU. Without it, the ECU
  loses its O2 signal and logs a fault.
- **Power:** switched 12 V, so it's only on with the ignition, plus a solid
  ground. Never run the sensor in a running exhaust while it's unpowered,
  because soot ruins it.
- **To the Mac:** the controller's USB or serial output goes into a second USB
  port. The app will read it and record it next to the ECU data. Tell me which
  controller you have and I'll add support for it.
