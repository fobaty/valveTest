# Universal Throttle Valve Controller

Firmware for driving a throttle valve actuator with a stepper motor on a
**Raspberry Pi Pico 2 W (RP2350)**.

The board brings up its own Wi-Fi access point and serves a small web UI that
lets you open/close the valve, run homing, jog the actuator manually, and read
back the current position.

- MCU: RP2350 (Pico 2 W), target `thumbv8m.main-none-eabihf`
- Wi-Fi: on-board CYW43439
- Actuator: stepper motor through an external driver (STEP / DIR / ENABLE)
- Position feedback: home/limit switch only (no encoder)

---

## Components

| Component | Purpose |
|---|---|
| Raspberry Pi Pico 2 W | Control |
| Stepper driver (TB6600, Rorze RD-021M8, ...) | Power stage |
| Stepper motor | Valve actuator |
| Limit switch (normally open) | Homing / reference position |
| Motor power supply | Motor power (typically 12–48 V) |
| USB cable | Flashing and board logic power |

> Motor power is supplied to the driver from a **separate** power supply.
> The driver PSU `GND` and the Pico 2 W `GND` **must be connected** — otherwise
> STEP/DIR/ENABLE will not work.

---

## Pinout

All signals are 3.3 V logic, going from the Pico 2 W to the driver.

| Signal | Pico 2 W GPIO | Pico physical pin | Driver |
|---|---|---|---|
| STEP | GP2 | 4 | STEP / PUL / CLK |
| DIR | GP3 | 5 | DIR |
| ENABLE | GP4 | 6 | ENA (active low) |
| HOME (limit switch) | GP5 | 7 | — (to GND when triggered) |
| GND | GND | 3, 8, 13, 18, 23, 28, 33, 38 | GND (logic ground, opto `-`) |

`ENABLE` is **active low**: low = motor held (energized), high = motor released.
This is handled in firmware; no change needed.

### Pins you must NOT use

On the Pico 2 W the CYW43439 wireless pins are used internally and are not
broken out:

| GPIO | Internal function |
|---|---|
| GP23 | WL_ON / radio power |
| GP24 | DATA (SPI) |
| GP25 | CS |
| GP29 | CLK |

Free to use (apart from the ones above): GP0–GP22, GP26–GP28.

---

## Wiring diagram

```
         Pico 2 W                                  Stepper driver
       ┌───────────┐                              ┌──────────────────────────┐
       │           │                              │                          │
       │  GP2  ────┼──────── STEP ───────────────►│ PUL+ / STEP              │
       │  GP3  ────┼──────── DIR ────────────────►│ DIR+                     │
       │  GP4  ────┼──────── ENABLE ─────────────►│ ENA+                     │
       │           │                              │                          │
       │  GND  ────┼──────── GND ────────────────►│ PUL- / DIR- / ENA- (GND) │
       │           │                              │                          │
       │  GP5  ────┼────┐                         │       A+ A- B+ B- ───────┼──► Motor
       │           │    │                         │                          │
       │  GND  ────┼────┴──[ limit switch ]       │   V+ / GND  ◄────────────┼── PSU 12–48 V
       └───────────┘                              └──────────────────────────┘
```

- The limit switch is wired between **GP5** and **GND**. The internal pull-up to
  3.3 V is enabled (`Pull::Up`), so the line idles at "1" and is pulled to ground
  ("0") when the switch is closed.
- Connect the driver opto common wires (`PUL-`, `DIR-`, `ENA-`) to the Pico
  `GND` — this is the logic common (common cathode). If the driver has a
  dedicated logic `GND` pin, connect it to the Pico `GND` as well.
- If your driver inputs are common-anode (active "+"), connect the Pico signals
  to `PUL+/DIR+/ENA+` and tie `PUL-/DIR-/ENA-` to the Pico `GND`.

> Direction (which way opens vs. closes) is set by the level on `DIR`. If it is
> inverted, swap any one pair of motor phase wires **or** flip the level in
> `src/motor.rs` (`step_open` / `step_close`).

---

## Limit switch (homing)

```
   GP5 ──────────┬──────────────┐
                 │            [ ]  switch contacts
   GND ──────────┴──────────────┘
```

- Idle (not pressed): GP5 = 1 (pulled up).
- Triggered: GP5 = 0.
- Homing: if the switch is already pressed the motor first backs off, then moves
  slowly towards the closing direction until the switch triggers, after which
  the position counter is zeroed.

---

## Wi-Fi and web UI

After power-up the board brings up an **open** access point:

| Parameter | Value |
|---|---|
| SSID | `ValveController` |
| Channel | 6 |
| Password | — (open network) |
| Board IP | `192.168.4.1` |

Connect to the `ValveController` network from a phone or laptop and open
**http://192.168.4.1/**.

### Endpoints

| Path | Action |
|---|---|
| `/` | Page with the current position (refresh) |
| `/open` | Fully open (to `MAX_TRAVEL_STEPS`) |
| `/close` | Fully close (to 0) |
| `/home` | Home using the limit switch |
| `/jog-open` | +200 steps |
| `/jog-close` | −200 steps |

Position is shown as a percentage and in steps.

---

## Build and flash

A stable Rust toolchain with the `thumbv8m.main-none-eabihf` target is required
(pinned in `rust-toolchain.toml`).

```bash
# build
cargo build --release

# flash via debug probe (probe-rs, SWD)
cargo install probe-rs-tools      # once
cargo run --release               # runner is configured in .cargo/config.toml
```

Alternative — flash over USB (BOOTSEL):

```bash
# produce a UF2 (needs elf2uf2 / picotool)
picotool uf2 convert target/thumbv8m.main-none-eabihf/release/throttle-valve-controller out.uf2
# then: hold BOOTSEL, plug in USB, copy out.uf2 to the RPI-RP2 drive
```

For debug output over RTT, keep a probe connected and watch the `defmt` logs.

---

## Configuration

Main parameters live in `src/main.rs`:

| Constant | Purpose | Default |
|---|---|---|
| `AP_SSID` | Access point name | `ValveController` |
| `AP_CHANNEL` | Wi-Fi channel | `6` |
| `AP_IP` | Board IP address | `192.168.4.1` |
| `MAX_TRAVEL_STEPS` | Full valve travel in steps | `20000` |

- `MAX_TRAVEL_STEPS` **must** be tuned to match the mechanics, taking the
  driver microstepping into account (the driver DIP switches set the step
  division).
- Speed and the minimum STEP pulse width are set in `src/motor.rs`
  (`step_delay_us`, `STEP_PULSE_US`).

---

## Project structure

| File | Purpose |
|---|---|
| `src/main.rs` | Init, Wi-Fi AP, actuator pin wiring |
| `src/motor.rs` | Stepper logic (open/close/home/jog) |
| `src/web.rs` | HTTP server and web UI |
| `memory.x` | RP2350 memory map and boot block |
| `build.rs` | Passes `link.x`/`defmt.x` to the linker |
| `cyw43-firmware/` | CYW43439 Wi-Fi chip firmware |
| `.cargo/config.toml` | Target and runner (`probe-rs --chip RP235x`) |
