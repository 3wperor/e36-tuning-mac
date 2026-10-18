# e36-tuning-mac

Native macOS (Apple Silicon) tools for the BMW E36's **Siemens MS41** engine
ECU (DME), for M52/S52 engines. No Windows or VM needed.

**Current stage: read-only diagnostics.** The tool can read the ECU's identity
and its fault codes, and clear those codes. Nothing here writes to the ECU's
flash yet. See [docs/PLAN.md](docs/PLAN.md) for where this is heading
(logging, map editing, flashing).

## Install (on the Mac)

```sh
# Install Rust once
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh

git clone https://github.com/3wperor/e36-tuning-mac
cd e36-tuning-mac
cargo build --release
```

The program is `target/release/e36`.

## Use

Hook up the cable as described in [docs/HARDWARE.md](docs/HARDWARE.md), then
turn the ignition to position II with the engine off.

```sh
./target/release/e36 ports                   # find the cable
./target/release/e36 ident --trace           # DME part number + raw bytes
./target/release/e36 faults --trace          # stored fault codes (raw)
./target/release/e36 clear-faults            # shows faults, asks, then erases
```

The port is detected automatically when only one USB serial cable is plugged
in. Otherwise pass `--port /dev/cu.usbserial-XXXX` or set `E36_PORT`.

Keep the `--trace` output from the first runs on the car. Those bytes are what
fault-code decoding and the next stages get built and tested against.

## Layout

```
crates/ds2   BMW DS2 protocol library: frames, echo handling, MS41 commands,
             serial cable, and a mock ECU for tests
crates/e36   the `e36` command-line tool
docs/        plan and hardware notes
```

`cargo test` runs the whole protocol test suite against the mock ECU; no car
is needed.
