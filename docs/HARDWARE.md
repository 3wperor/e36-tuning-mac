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
