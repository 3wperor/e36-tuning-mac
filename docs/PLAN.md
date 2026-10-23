# Plan

Goal: a native Apple Silicon macOS tool to **read, log, tune and flash** the
Siemens MS41 DME in an E36. No Windows, no VM.

Target car: 1997 Euro E36 with an M52B25 engine, which means an MS41 DME. The
exact variant is still unknown; `e36 ident` reads it from the car.

## Architecture

| Layer | What it does | Status |
|---|---|---|
| Transport | USB serial at 9600 baud, 8E1, handling of the K-line echo, timeouts, line draining after bad frames | done (`crates/ds2/src/serial.rs`, `session.rs`) |
| Live polling | Self-adjusting request pacing for a running engine; errors retried; app reconnects by itself | done (`poll.rs`); tested against a simulated busy, noisy ECU |
| DS2 protocol | `[addr][len][payload][xor]` frames; the DME is at address `0x12` | done (`frame.rs`) |
| Diagnostics | ECU identity, read and clear fault codes | done (`ms41/mod.rs`) |
| Simulator | Pretend MS41 for demo mode and tests | done (`sim.rs`) |
| Fault decoding | Fault codes to text (`ms41/fault_codes.rs`); reply layout from a forum capture | first version done; layout **unverified** |
| Logger | Live data block `0B 03`: gauges, chart, raw byte view, CSV export | first version done; value layout **unverified** |
| VANOS view | On/off indicator, timeline, switch log with RPM, bit finder to locate the flag | done; flag location found per car with the finder |
| Knock & misfire | Global knock retard (`0B 93`, raw, units unverified) polled ~1/s; fault watch every 15 s with per-cylinder misfire (codes 238-243) and knock-sensor (57/59) status; events recorded | done; knock request and units to confirm on the car; per-cylinder knock retard still unknown |
| Wideband | Read a wideband O2 controller over its own USB serial port | step 2 |
| ECU definitions | Import the community TunerPro XDF files (tables, axes, scaling) | step 3 |
| Map editor | 2D/3D tables, comparison against stock, undo, version history | step 3 |
| Checksums | Recalculate MS41 checksums; refuse to flash if they don't match | step 3 |
| Flasher | Identity check, full backup, erase, write, verify | step 4 |
| Analysis | Fuel trim and wideband correction suggestions, knock per cylinder | step 5 |
| GUI | Native window using egui (`crates/e36-app`) | first version done |
| Tuning practice | Simulated M52 (`crates/tunesim`) with editable fuel/ignition/VANOS maps, dyno pull, cam position comparison, ITB runner length, fuel trims, seven scenarios with checks | done; the map editor is the starting point for real map editing in step 3 |

## Build order

1. **Read-only CLI.** Connect, read the identity, read and clear faults.
   Proves the cable, the 20-pin adapter and the protocol on the Mac. *(done;
   needs testing on the car)*
2. **Logger.** *(first version done: `e36 live` and the app)* Confirm
   the live data layout on the car, then add wideband input and more channels.

   egui was chosen over Tauri for the GUI because it's plain Rust: no Node or
   web tooling, just `cargo run`.
3. **Read the full binary and back it up.** Then XDF import, the map editor and
   checksums.
4. **Flashing**, with the safety plan below.
5. **Analysis tools.**

## Flashing without a spare ECU

There's no spare ECU to practise on, so the car's own ECU is the test unit.
That's riskier, and every step below exists to make a failed flash
recoverable rather than fatal.

1. **Backups before anything else.** Read the full ECU contents twice and
   compare them byte for byte; they must match. Store copies in three places
   (Mac, cloud, USB stick). The original file is never modified, so every tune
   is a copy.
2. **Check our reader against an established tool.** Read the same ECU once
   with the established community tool, in a Windows VM if necessary. Our dump
   must match it byte for byte before our writer is trusted.
3. **Prove the recovery path first.** The MS41's processor has a bootstrap mode
   ("bootmode") that lets it be reflashed even when its program is corrupted.
   Before the **first write**, we get a bootmode **read** working on your ECU.
   That's done with the ECU out of the car on the desk, powered by a cheap 12V
   supply or the car battery. Once that works, a failed flash means an
   afternoon of recovery, not a new ECU.
4. **Write as little as possible.** Where possible, write only the calibration
   (the maps) and leave the program code alone. That makes the write smaller
   and faster, with less to go wrong.
5. **Refuse unsafe flashes.** The tool won't flash if:
   - the part or software number doesn't match the backup
   - the checksums are wrong
   - the file size is wrong
   - battery voltage is below about 12.5 V
6. **During a flash:**
   - battery charger connected
   - lights, fan and radio off; doors shut so the interior light stays off
   - Mac plugged in, with sleep blocked (`caffeinate`)
   - wired connection only
7. **Verify after writing.** Read back and compare with what was written before
   turning the ignition off.
8. **Keep the first change tiny.** For example, one value in a map you can
   easily check. That proves the whole write path end to end before any real
   tuning.

## Open questions

- **Is your ECU flashable?** The MS4X wiki notes that some MS41 units have a
  flash chip that can't be reprogrammed; the most common ones can. The part
  number from `e36 ident` should tell us which you have.
- **The live data layout.** Record ~30 seconds in the app (idle, a few revs,
  idle) and share the CSV. The raw bytes in it confirm or correct each value.
- Exact DME part number and software version (`e36 ident` will show it).
- How MS41 fault records are laid out. The decoder assumes a count byte, then
  8-byte records starting with the code. Capture `e36 faults --trace` from the
  car, ideally once with a known fault (e.g. an unplugged sensor) and once
  after clearing, to confirm it and to decode the other bytes in each record.
- **Knock retard.** Does your ECU answer `0B 93`, and what do its bytes mean?
  A recording that includes a knock event would let us calibrate the units.
  Per-cylinder knock retard lives in ECU memory at addresses that differ by
  software version; reading it needs your software number from `e36 ident`.
- **Misfire detection on MS41.0.** Community reports say the Euro MS41.0 has
  no misfire detection logic (it arrived with the US MS41.1); others dispute
  it. If no misfire code ever appears, that's probably why.
