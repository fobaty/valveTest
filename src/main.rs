//! # Universal Throttle Valve Controller
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
//!
//! The stepper driver (TB6600, Rorze RD-021M8, ...) receives STEP/DIR/ENABLE and
//! drives the motor. Position is tracked in step counts; no analog feedback is used.

#![no_std]
#![no_main]
#![allow(async_fn_in_trait)]

use defmt::*;
use defmt_rtt as _;
use panic_probe as _;

use cyw43::aligned_bytes;
use cyw43_pio::{PioSpi, DEFAULT_CLOCK_DIVIDER};
use embassy_executor::Spawner;
use embassy_net::{Config, Ipv4Address, Ipv4Cidr, StackResources, StaticConfigV4};
use embassy_rp::clocks::RoscRng;
use embassy_rp::gpio::{Input, Level, Output, Pull};
use embassy_rp::peripherals::{DMA_CH0, DMA_CH1, PIO0};
use embassy_rp::pio::{InterruptHandler as PioInterruptHandler, Pio};
use embassy_rp::{bind_interrupts, dma};
use static_cell::StaticCell;

mod motor;
mod web;

use motor::StepperMotor;
use web::run_web_server;

const AP_SSID: &str = "ValveController";
const AP_CHANNEL: u8 = 6;
const AP_IP: [u8; 4] = [192, 168, 4, 1];

/// Full valve travel in steps. Tune this to match the mechanics.
const MAX_TRAVEL_STEPS: i32 = 20000;

bind_interrupts!(struct Irqs {
    PIO0_IRQ_0 => PioInterruptHandler<PIO0>;
    DMA_IRQ_0 => dma::InterruptHandler<DMA_CH0>, dma::InterruptHandler<DMA_CH1>;
});

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

#[embassy_executor::main]
async fn main(spawner: Spawner) {
    info!("Universal throttle valve controller starting");

    let p = embassy_rp::init(Default::default());
    let mut rng = RoscRng;

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
    info!("AP '{}' up at http://192.168.4.1/", AP_SSID);

    // --- Stepper motor and home / limit switch ---
    let step_pin = Output::new(p.PIN_2, Level::Low);
    let dir_pin = Output::new(p.PIN_3, Level::Low);
    let enable_pin = Output::new(p.PIN_4, Level::High);
    let home_sensor = Input::new(p.PIN_5, Pull::Up);

    let mut motor = StepperMotor::new(step_pin, dir_pin, enable_pin);
    motor.set_max_steps(MAX_TRAVEL_STEPS);

    run_web_server(stack, &mut motor, &home_sensor).await;
}
