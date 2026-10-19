# Step Motor Tester

Firmware for driving a throttle valve actuator with a stepper motor on a
**Raspberry Pi Pico 2 W (RP2350)**.

The board brings up its own Wi-Fi access point and serves a small web UI that
lets you open/close the valve, run homing, jog the actuator manually, and read
back its position, along with real-time indicators for limit switches, motor direction LEDs, and emergency stop handling.

- MCU: RP2350 (Pico 2 W), target `thumbv8m.main-none-eabihf`
- Wi-Fi: on-board CYW43439
- Actuator: stepper motor through an external driver (Rorze RD-021M8, STEP / DIR)
- Position feedback: home/limit switch only (no encoder)

---

## Components

| Component | Purpose |
|---|---|
| Raspberry Pi Pico 2 W | Control |
| Stepper driver (Rorze RD-021M8) | Power stage |
| Stepper motor | Valve actuator |
| Limit switch (normally open) | Homing / reference position |
| Status LEDs | Limit switch triggered, Open active, Close active |
| Physical buttons | Open, Close, Home, Jog +, Jog -, Emergency Stop |
| Motor power supply | 24 V into the driver `VCC` (RD-021M8 range: 10–40 V) |
| USB cable | Flashing and board logic power |

> Motor power (24 V) is supplied to the driver from a **separate** power supply
> and goes only to the driver `VCC`/`GND`. The RD-021M8 clock/direction inputs
> are **photo-isolated**, so the Pico logic side does not need to share ground
> with the motor supply — the Pico drives the opto LEDs from its own `+5 V` pin.

---

## Pinout

Logic is 3.3 V from the Pico 2 W; the opto anodes of the driver are fed from the
Pico `+5 V` rail.

| Signal | Pico 2 W GPIO | Pico physical pin | Description |
|---|---|---|---|
| STEP (clock) | GP2 | 4 | `-CW (CLK)` driver input |
| DIR (direction) | GP3 | 5 | `-CCW (UD)` driver input |
| ENABLE | GP4 | 6 | Driver enable (active low) |
| HOME (limit switch) | GP5 | 7 | Limit switch input (pulled up, pulls to GND when closed) |
| Limit switch LED | GP6 | 9 | LED lights up when limit switch is triggered |
| Open active LED | GP7 | 10 | LED lights up when motor moves / is active in Open direction |
| Close active LED | GP8 | 11 | LED lights up when motor moves / is active in Close direction |
| Button: Open | GP9 | 12 | Physical Open button (active low) |
| Button: Close | GP10 | 14 | Physical Close button (active low) |
| Button: Home | GP11 | 15 | Physical Home button (active low) |
| Button: + (Jog Open) | GP12 | 16 | Physical Jog +200 steps button (active low) |
| Button: - (Jog Close) | GP13 | 17 | Physical Jog -200 steps button (active low) |
| Button: Emergency Stop | GP14 | 19 | Physical Emergency Stop button (active low) |
| opto anode (common) | +5 V (VBUS) | 40 | `+CW (CLK)` and `+CCW (UD)` |

The RD-021M8 clock/direction inputs are **photo-isolated, negative logic**: the
opto LED conducts when the driven `-` terminal is pulled low by the GPIO. The
`+CW`/`+CCW` anodes are wired straight to the Pico `+5 V` pin and `-CW`/`-CCW`
to `GP2`/`GP3` (no series resistors, no transistors — the driver's internal
resistor limits the LED current to ~12 mA at 5 V).

### Pins you must NOT use

On the Pico 2 W the CYW43439 wireless pins are used internally and are not
broken out:

| GPIO | Internal function |
|---|---|
| GP23 | WL_ON / radio power |
| GP24 | DATA (SPI) |
| GP25 | CS |
| GP29 | CLK |

Free to use (apart from the ones above): GP0, GP1, GP15–GP22, GP26–GP28.

---

## Wiring diagram

```
         Pico 2 W                              Rorze RD-021M8
       ┌───────────┐                            ┌──────────────────────────┐
       │           │                            │                          │
       │  GP2  ────┼──── STEP ─────────────────►│ -CW (CLK)                │
       │  GP3  ────┼──── DIR  ─────────────────►│ -CCW (UD)                │
       │           │                            │                          │
       │  +5V  ────┼───────────────────────────►│ +CW (CLK), +CCW (UD)     │
       │           │                            │                          │
       │  GP5  ────┼──┐                         │  A  Ā  B  B̄ ────────────┼──► Motor
       │  GP6  ────┼──┼──[ Limit LED ]          │                          │
       │  GP7  ────┼──┼──[ Open LED ]           │  VCC / GND ◄─────────────┼── PSU 24 V
       │  GP8  ────┼──┼──[ Close LED ]          └──────────────────────────┘
       │  GP9-14 ──┼──┴─[ Physical Buttons (Open, Close, Home, +, -, E-Stop) ]
       └───────────┘
```

- `+CW (CLK)` and `+CCW (UD)` are tied together to the Pico `+5 V` pin (common
  anode). `-CW (CLK)` and `-CCW (UD)` go to `GP2` (STEP) and `GP3` (DIR).
- No series resistors or transistors are used; the opto LED current is limited
  by the driver's internal resistor (~270 Ω → ≈12 mA at 5 V).
- The RD-021M8 DIP switches must select **1CK** mode, so `CLK` = step and
  `UD` = direction. Set microstepping with the other DIP switches
  (OFF/OFF = 1/8, OFF/ON = 1/4, ON/OFF = 1/2, ON/ON = 1/1).
- Motor current is set by the external resistor on `R-A`/`R-B` (0.1–1.5 A/phase);
  with no resistor the current is at maximum.
- The limit switch is wired between **GP5** and **GND**. The internal pull-up to
  3.3 V is enabled (`Pull::Up`), so the line idles at "1" and is pulled to ground
  ("0") when the switch is closed.
- Motor power is 24 V into `VCC` (`DC10-40V`) / `GND`.

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
- LED indicator on **GP6** lights up whenever the limit switch is triggered.

---

## Wi-Fi and web UI

After power-up the board brings up an **open** access point:

| Parameter | Value |
|---|---|
| SSID | `StepMotorTester` |
| Channel | 6 |
| Password | — (open network) |
| Board IP | `192.168.4.1` |

Connect to the `StepMotorTester` network from a phone or laptop and open
**http://192.168.4.1/**. The firmware runs a small DHCP server, so clients get
an IP address automatically (no static configuration needed).

### Web UI Features
- Current position (percentage & steps) and motion status.
- **Limit Switch status indicator** (Triggered / Idle).
- **Open Active / Close Active LED state indicators**.
- **Emergency Stop banner & Reset E-Stop button** (immediately halts motor and prevents movement until reset).
- **Motor & Travel Settings** (max steps lock, max steps, steps/deg, gearbox ratio) are written to the on-board flash on **Save Settings** and restored automatically after power-up.

### Endpoints

| Path | Action |
|---|---|
| `/` | Page with status, indicators, and controls |
| `/open` | Fully open (to `MAX_TRAVEL_STEPS`) |
| `/close` | Fully close (to 0) |
| `/home` | Home using the limit switch |
| `/jog-open` | +200 steps |
| `/jog-close` | −200 steps |
| `/reset-estop` | Clear emergency stop state |

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
picotool uf2 convert target/thumbv8m.main-none-eabihf/release/step-motor-tester out.uf2
# then: hold BOOTSEL, plug in USB, copy out.uf2 to the RPI-RP2 drive
```

For debug output over RTT, keep a probe connected and watch the `defmt` logs.

---

## Configuration

Main parameters live in `src/main.rs`:

| Constant | Purpose | Default |
|---|---|---|
| `AP_SSID` | Access point name | `StepMotorTester` |
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
| `src/main.rs` | Init, Wi-Fi AP, pin wiring, sensors & buttons tasks |
| `src/motor.rs` | Stepper logic (open/close/home/jog, LED updates, E-stop) |
| `src/web.rs` | HTTP server, web UI with indicators and E-stop controls |
| `src/dhcp.rs` | Minimal DHCP server for the AP |
| `memory.x` | RP2350 memory map and boot block |
| `build.rs` | Passes `link.x`/`defmt.x` to the linker |
| `cyw43-firmware/` | CYW43439 Wi-Fi chip firmware |
| `.cargo/config.toml` | Target and runner (`probe-rs --chip RP235x`) |
