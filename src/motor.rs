use core::sync::atomic::{AtomicBool, AtomicI32};

use embassy_rp::gpio::{Input, Output};
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::signal::Signal;
use embassy_time::{Duration, Instant, Timer};

/// Full valve travel in steps. Tune this to match the mechanics.
pub const MAX_TRAVEL_STEPS: i32 = 20000;

/// Minimum STEP pulse width, in microseconds.
const STEP_PULSE_US: u64 = 3;

/// Safety budgets for homing. Without them a missing or stuck limit switch
/// would spin the search loops forever and stall every other task.
const HOME_BACKOFF_MAX: i32 = 4_000;
const HOME_SEARCH_MAX: i32 = 30_000;
const HOME_TIMEOUT: Duration = Duration::from_secs(45);

/// Movement request handed from the web server to the motor task.
#[derive(Clone, Copy)]
pub enum Command {
    Open,
    Close,
    Home,
    Jog(i32),
}

/// Newest command waiting to be executed.
///
/// `Signal` holds a single slot, so a burst of button presses is coalesced
/// instead of building a backlog of queued moves.
pub static COMMANDS: Signal<CriticalSectionRawMutex, Command> = Signal::new();

/// Motor state published for the web UI: written by the motor task, read by
/// the web server at any time without borrowing the motor itself.
pub static POSITION: AtomicI32 = AtomicI32::new(0);
pub static BUSY: AtomicBool = AtomicBool::new(false);

/// A step/dir stepper motor driven through an external driver.
///
/// The motor keeps a software position counter in steps. Position `0` is the
/// home position; increasing values move the valve towards open.
pub struct StepperMotor<'d> {
    step_pin: Output<'d>,
    dir_pin: Output<'d>,
    enable_pin: Output<'d>,
    position: i32,
    max_steps: i32,
    step_delay_us: u64,
}

impl<'d> StepperMotor<'d> {
    pub fn new(step_pin: Output<'d>, dir_pin: Output<'d>, enable_pin: Output<'d>) -> Self {
        let mut motor = Self {
            step_pin,
            dir_pin,
            enable_pin,
            position: 0,
            max_steps: MAX_TRAVEL_STEPS,
            step_delay_us: 300,
        };
        motor.disable();
        motor
    }

    pub fn enable(&mut self) {
        self.enable_pin.set_low();
    }

    pub fn disable(&mut self) {
        self.enable_pin.set_high();
    }

    pub fn set_speed(&mut self, step_delay_us: u64) {
        self.step_delay_us = step_delay_us;
    }

    pub fn position(&self) -> i32 {
        self.position
    }

    async fn pulse(&mut self) {
        self.step_pin.set_high();
        Timer::after(Duration::from_micros(STEP_PULSE_US)).await;
        self.step_pin.set_low();
        Timer::after(Duration::from_micros(self.step_delay_us)).await;
    }

    /// Move one step in the opening direction, clamping at the travel limit.
    async fn step_open(&mut self) {
        if self.position >= self.max_steps {
            return;
        }
        self.dir_pin.set_high();
        self.pulse().await;
        self.position += 1;
    }

    /// Move one step in the closing direction, clamping at the home position.
    async fn step_close(&mut self) {
        if self.position <= 0 {
            return;
        }
        self.dir_pin.set_low();
        self.pulse().await;
        self.position -= 1;
    }

    /// Drive the valve open until it reaches the configured limit.
    pub async fn open(&mut self) {
        self.enable();
        while self.position < self.max_steps {
            self.step_open().await;
        }
        self.disable();
    }

    /// Drive the valve closed until it reaches the home position.
    pub async fn close(&mut self) {
        self.enable();
        while self.position > 0 {
            self.step_close().await;
        }
        self.disable();
    }

    /// Move to an absolute position in steps, clamped to `[0, max_steps]`.
    pub async fn move_to(&mut self, target: i32) {
        let target = target.clamp(0, self.max_steps);
        self.enable();
        while self.position != target {
            if self.position < target {
                self.step_open().await;
            } else {
                self.step_close().await;
            }
        }
        self.disable();
    }

    /// Move relative to the current position by `steps` (positive = open).
    pub async fn jog(&mut self, steps: i32) {
        let target = self.position + steps;
        self.move_to(target).await;
    }

    /// Search for the home position using a limit switch.
    ///
    /// The motor first backs off the switch if it is already pressed, then
    /// closes until the switch triggers and finally re-zeroes the counter.
    ///
    /// Both loops are bounded by a step budget and a wall-clock timeout: a
    /// missing or stuck switch aborts the search instead of spinning forever
    /// and wedging the task that owns the motor.
    pub async fn home(&mut self, sensor: &Input<'d>) {
        let started = Instant::now();
        let before = self.position;
        self.enable();

        // Back off if the switch is already active.
        let mut backoff = 0;
        while sensor.is_low() && backoff < HOME_BACKOFF_MAX && started.elapsed() < HOME_TIMEOUT {
            self.dir_pin.set_high();
            self.pulse().await;
            self.position += 1;
            backoff += 1;
        }

        // Approach the switch slowly for repeatability.
        let saved = self.step_delay_us;
        self.set_speed(saved.max(800));
        let mut search = 0;
        while sensor.is_high() && search < HOME_SEARCH_MAX && started.elapsed() < HOME_TIMEOUT {
            self.dir_pin.set_low();
            self.pulse().await;
            self.position -= 1;
            search += 1;
        }
        self.set_speed(saved);

        if sensor.is_low() {
            self.position = 0;
        } else {
            // The counter is now untrustworthy, but restoring it keeps it
            // inside [0, max_steps] so open/close still behave.
            log::warn!(
                "homing aborted after {} steps: no edge on the limit switch (GP5)",
                search
            );
            self.position = before.clamp(0, self.max_steps);
        }
        self.disable();
    }
}
