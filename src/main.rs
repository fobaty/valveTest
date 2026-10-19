//! # Step Motor Tester
//!
//! Firmware for a stepper-driven throttle valve on a Raspberry Pi Pico 2W (RP2350).
//! The board brings up a Wi-Fi access point and serves a small web UI to open,
//! close, home and jog the valve, and to read back its position.
//!
//! ## Default pinout
//! - GPIO 2  -> driver STEP
//! - GPIO 3  -> driver DIR
//! - GPIO 4  -> driver ENABLE (active low)
//! - GPIO 5  -> home / limit switch (pulled up, shorts to GND when triggered)
//! - GPIO 6  -> limit switch LED indicator
//! - GPIO 7  -> Open state active LED indicator
//! - GPIO 8  -> Close state active LED indicator
//! - GPIO 9  -> Physical button: Open
//! - GPIO 10 -> Physical button: Close
//! - GPIO 11 -> Physical button: Home
//! - GPIO 12 -> Physical button: + (Jog Open)
//! - GPIO 13 -> Physical button: - (Jog Close)
//! - GPIO 14 -> Physical button: Emergency Stop

#![no_std]
#![no_main]
#![allow(async_fn_in_trait)]

use core::sync::atomic::Ordering;

use defmt::*;
use defmt_rtt as _;
use panic_probe as _;

use cyw43::aligned_bytes;
use cyw43_pio::{PioSpi, DEFAULT_CLOCK_DIVIDER};
use embassy_executor::Spawner;
use embassy_net::{Config, Ipv4Address, Ipv4Cidr, Stack, StackResources, StaticConfigV4};
use embassy_rp::clocks::RoscRng;
use embassy_rp::flash::Flash;
use embassy_rp::gpio::{Input, Level, Output, Pull};
use embassy_rp::peripherals::{DMA_CH0, DMA_CH1, PIO0, USB};
use embassy_rp::pio::{InterruptHandler as PioInterruptHandler, Pio};
use embassy_rp::{bind_interrupts, dma};
use static_cell::StaticCell;

mod dhcp;
mod motor;
mod settings;
mod web;

use motor::{Command, StepperMotor, BUSY, COMMANDS, POSITION};
use web::run_web_server;

const AP_SSID: &str = "StepMotorTester";
const AP_CHANNEL: u8 = 6;
const AP_IP: [u8; 4] = [192, 168, 4, 1];

bind_interrupts!(struct Irqs {
    PIO0_IRQ_0 => PioInterruptHandler<PIO0>;
    DMA_IRQ_0 => dma::InterruptHandler<DMA_CH0>, dma::InterruptHandler<DMA_CH1>;
    USBCTRL_IRQ => embassy_rp::usb::InterruptHandler<USB>;
});

/// Log bridge: everything logged with the `log` facade is printed on the USB
/// serial port (CDC-ACM), so logs can be viewed on the host with a serial
/// terminal such as `screen`/`cu`.
#[embassy_executor::task]
async fn logger_task(driver: embassy_rp::usb::Driver<'static, USB>) {
    embassy_usb_logger::run!(4096, log::LevelFilter::Info, driver);
}

#[embassy_executor::task]
async fn cyw43_task(
    runner: cyw43::Runner<'static, cyw43::SpiBus<Output<'static>, PioSpi<'static, PIO0, 0>>>,
) -> ! {
    runner.run().await
}

#[embassy_executor::task]
async fn net_task(mut runner: embassy_net::Runner<'static, cyw43::NetDriver<'static>>) -> ! {
    runner.run().await
}

/// Answer DHCP requests from clients that join the AP, so they get an address
/// automatically instead of needing a static IP.
#[embassy_executor::task]
async fn dhcp_task(stack: Stack<'static>, server: Ipv4Address) -> ! {
    dhcp::run_dhcp_server(stack, server).await
}

/// Owns the motor and runs one command at a time.
///
/// Keeping the move here (instead of inline in the web server) is what lets the
/// HTTP handler answer immediately while the valve is still travelling, and
/// isolates a long-running homing sweep from the rest of the firmware.
#[embassy_executor::task]
async fn motor_task(mut motor: StepperMotor<'static>) {
    loop {
        let command = COMMANDS.wait().await;
        if motor::EMERGENCY_STOP_ACTIVE.load(Ordering::Relaxed) {
            log::warn!("Command ignored because emergency stop is active");
            continue;
        }
        BUSY.store(true, Ordering::Relaxed);

        match command {
            Command::Open => motor.open().await,
            Command::Close => motor.close().await,
            Command::Home => motor.home().await,
            Command::Jog(steps) => motor.jog(steps).await,
        }

        POSITION.store(motor.position(), Ordering::Relaxed);
        BUSY.store(false, Ordering::Relaxed);
    }
}

/// Monitors the limit switch sensor and drives its indicator LED.
#[embassy_executor::task]
async fn sensors_task(home_sensor: Input<'static>, mut limit_led: Output<'static>) {
    loop {
        let active = home_sensor.is_low();
        motor::LIMIT_SWITCH_ACTIVE.store(active, Ordering::Relaxed);
        if active {
            limit_led.set_high();
        } else {
            limit_led.set_low();
        }
        embassy_time::Timer::after(embassy_time::Duration::from_millis(10)).await;
    }
}

/// Polls physical buttons (Open, Close, Home, Jog +, Jog -, Emergency Stop) with debouncing.
#[embassy_executor::task]
async fn buttons_task(
    open_btn: Input<'static>,
    close_btn: Input<'static>,
    home_btn: Input<'static>,
    jog_open_btn: Input<'static>,
    jog_close_btn: Input<'static>,
    emergency_stop_btn: Input<'static>,
) {
    let mut prev_open = true;
    let mut prev_close = true;
    let mut prev_home = true;
    let mut prev_jog_open = true;
    let mut prev_jog_close = true;
    let mut prev_estop = true;

    loop {
        let open_pressed = open_btn.is_low();
        let close_pressed = close_btn.is_low();
        let home_pressed = home_btn.is_low();
        let jog_open_pressed = jog_open_btn.is_low();
        let jog_close_pressed = jog_close_btn.is_low();
        let estop_pressed = emergency_stop_btn.is_low();

        if estop_pressed && !prev_estop {
            log::warn!("Emergency stop button pressed!");
            motor::EMERGENCY_STOP_ACTIVE.store(true, Ordering::Relaxed);
        }

        if !estop_pressed && prev_estop && motor::EMERGENCY_STOP_ACTIVE.load(Ordering::Relaxed) {
            log::info!("Emergency stop released");
            motor::EMERGENCY_STOP_ACTIVE.store(false, Ordering::Relaxed);
        }

        if !motor::EMERGENCY_STOP_ACTIVE.load(Ordering::Relaxed) {
            if open_pressed && !prev_open {
                motor::COMMANDS.signal(Command::Open);
            }
            if close_pressed && !prev_close {
                motor::COMMANDS.signal(Command::Close);
            }
            if home_pressed && !prev_home {
                motor::COMMANDS.signal(Command::Home);
            }
            if jog_open_pressed && !prev_jog_open {
                motor::COMMANDS.signal(Command::Jog(200));
            }
            if jog_close_pressed && !prev_jog_close {
                motor::COMMANDS.signal(Command::Jog(-200));
            }
        }

        prev_open = open_pressed;
        prev_close = close_pressed;
        prev_home = home_pressed;
        prev_jog_open = jog_open_pressed;
        prev_jog_close = jog_close_pressed;
        prev_estop = estop_pressed;

        embassy_time::Timer::after(embassy_time::Duration::from_millis(20)).await;
    }
}

#[embassy_executor::main]
async fn main(spawner: Spawner) {
    let p = embassy_rp::init(Default::default());
    let mut rng = RoscRng;

    // --- USB serial logging (view with a serial terminal on the host) ---
    let usb_driver = embassy_rp::usb::Driver::new(p.USB, Irqs);
    spawner.spawn(unwrap!(logger_task(usb_driver)));
    log::info!("Step motor tester starting ({})", motor::FIRMWARE_VERSION);

    // --- Persistent settings in the on-board flash ---
    // Restore the travel configuration saved by the web UI (if any) before the
    // motor and web tasks start, so the limits are correct from the first move.
    let flash: settings::SettingsFlash = Flash::new_blocking(p.FLASH);
    settings::register(flash);
    match settings::load() {
        Some(saved) => {
            saved.apply();
            log::info!("restored settings from flash");
        }
        None => log::info!("no stored settings, using defaults"),
    }

    // --- CYW43 Wi-Fi chip (on-board on Pico 2W) ---
    let fw = aligned_bytes!("../cyw43-firmware/43439A0.bin");
    let clm = aligned_bytes!("../cyw43-firmware/43439A0_clm.bin");
    let nvram = aligned_bytes!("../cyw43-firmware/nvram_rp2040.bin");

    let pwr = Output::new(p.PIN_23, Level::Low);
    let cs = Output::new(p.PIN_25, Level::High);
    let mut pio = Pio::new(p.PIO0, Irqs);
    let spi = PioSpi::new(
        &mut pio.common,
        pio.sm0,
        DEFAULT_CLOCK_DIVIDER,
        pio.irq0,
        cs,
        p.PIN_24,
        p.PIN_29,
        dma::Channel::new(p.DMA_CH0, Irqs),
    );

    static STATE: StaticCell<cyw43::State> = StaticCell::new();
    let state = STATE.init(cyw43::State::new());
    let (net_device, mut control, runner) = cyw43::new(state, pwr, spi, fw, nvram).await;
    spawner.spawn(unwrap!(cyw43_task(runner)));

    control.init(clm).await;
    control
        .set_power_management(cyw43::PowerManagementMode::PowerSave)
        .await;

    // --- Bring up a Wi-Fi access point with a static IP ---
    control.start_ap_open(AP_SSID, AP_CHANNEL).await;

    let config = Config::ipv4_static(StaticConfigV4 {
        address: Ipv4Cidr::new(Ipv4Address::new(AP_IP[0], AP_IP[1], AP_IP[2], AP_IP[3]), 24),
        gateway: Some(Ipv4Address::new(AP_IP[0], AP_IP[1], AP_IP[2], AP_IP[3])),
        dns_servers: Default::default(),
    });

    static RESOURCES: StaticCell<StackResources<3>> = StaticCell::new();
    let resources = RESOURCES.init(StackResources::new());
    let seed = rng.next_u64();
    let (stack, runner) = embassy_net::new(net_device, config, resources, seed);
    spawner.spawn(unwrap!(net_task(runner)));

    stack.wait_config_up().await;
    log::info!(
        "AP '{}' up, static IP {}.{}.{}.{}",
        AP_SSID,
        AP_IP[0],
        AP_IP[1],
        AP_IP[2],
        AP_IP[3]
    );

    let server_ip = Ipv4Address::new(AP_IP[0], AP_IP[1], AP_IP[2], AP_IP[3]);
    spawner.spawn(unwrap!(dhcp_task(stack, server_ip)));

    // --- Stepper motor, sensors, LEDs and buttons ---
    let step_pin = Output::new(p.PIN_2, Level::Low);
    let dir_pin = Output::new(p.PIN_3, Level::Low);
    let enable_pin = Output::new(p.PIN_4, Level::High);
    let home_sensor = Input::new(p.PIN_5, Pull::Up);

    let limit_led = Output::new(p.PIN_6, Level::Low);
    let open_led = Output::new(p.PIN_7, Level::Low);
    let close_led = Output::new(p.PIN_8, Level::Low);

    let _open_btn = Input::new(p.PIN_9, Pull::Up);
    let _close_btn = Input::new(p.PIN_10, Pull::Up);
    let _home_btn = Input::new(p.PIN_11, Pull::Up);
    let _jog_open_btn = Input::new(p.PIN_12, Pull::Up);
    let _jog_close_btn = Input::new(p.PIN_13, Pull::Up);
    let _emergency_stop_btn = Input::new(p.PIN_14, Pull::Up);

    let motor = StepperMotor::new(step_pin, dir_pin, enable_pin, open_led, close_led);
    spawner.spawn(unwrap!(motor_task(motor)));
    spawner.spawn(unwrap!(sensors_task(home_sensor, limit_led)));
    // Physical buttons task disabled by default until buttons are physically wired to GND.
    // spawner.spawn(unwrap!(buttons_task(open_btn, close_btn, home_btn, jog_open_btn, jog_close_btn, emergency_stop_btn)));

    run_web_server(stack).await;
}
