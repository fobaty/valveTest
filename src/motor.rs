use embassy_rp::gpio::{Input, Output};
use embassy_time::{Duration, Timer};

/// Minimum STEP pulse width, in microseconds.
const STEP_PULSE_US: u64 = 3;

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
            max_steps: 20000,
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

    pub fn set_max_steps(&mut self, max_steps: i32) {
        self.max_steps = max_steps;
    }

    pub fn set_speed(&mut self, step_delay_us: u64) {
        self.step_delay_us = step_delay_us;
    }

    pub fn position(&self) -> i32 {
        self.position
    }

    pub fn percent(&self) -> f32 {
        if self.max_steps <= 0 {
            return 0.0;
        }
        (self.position as f32 / self.max_steps as f32 * 100.0).clamp(0.0, 100.0)
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
    pub async fn home(&mut self, sensor: &Input<'d>) {
        self.enable();

        // Back off if the switch is already active.
        while sensor.is_low() {
            self.dir_pin.set_high();
            self.pulse().await;
            self.position += 1;
        }

        // Approach the switch slowly for repeatability.
        let saved = self.step_delay_us;
        self.set_speed(self.step_delay_us.max(800));
        while sensor.is_high() {
            self.dir_pin.set_low();
            self.pulse().await;
            self.position -= 1;
        }
        self.set_speed(saved);

        self.position = 0;
        self.disable();
    }
}
