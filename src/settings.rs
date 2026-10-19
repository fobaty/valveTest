//! Persist the user-tunable travel settings in the on-board QSPI flash so they
//! survive a power cycle.
//!
//! The last 4 KiB sector of the 4 MiB flash is reserved for a single settings
//! record. The record carries a magic value, a version byte and a checksum so a
//! partially written or erased sector is detected and falls back to defaults
//! instead of loading garbage into the motion limits.

use core::cell::RefCell;
use core::sync::atomic::Ordering;

use embassy_rp::flash::{Blocking, Flash, ERASE_SIZE};
use embassy_rp::peripherals::FLASH;
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::blocking_mutex::Mutex;

use crate::motor;

/// Physical flash size of a Pico 2W (RP2350) module.
pub const FLASH_SIZE: usize = 4 * 1024 * 1024;

pub type SettingsFlash = Flash<'static, FLASH, Blocking, FLASH_SIZE>;

/// Reserve the last sector, well above the program image (linked into 2 MiB).
const SETTINGS_OFFSET: u32 = (FLASH_SIZE - ERASE_SIZE) as u32;

const MAGIC: u32 = 0x534D_5453; // "STMS"
const VERSION: u8 = 1;
const RECORD_LEN: usize = 24;

/// Flash driver registered by `main`, shared with the web server that persists
/// settings on submit.
static FLASH: Mutex<CriticalSectionRawMutex, RefCell<Option<SettingsFlash>>> =
    Mutex::new(RefCell::new(None));

/// Hand the flash driver created in `main` to the settings store.
pub fn register(flash: SettingsFlash) {
    FLASH.lock(|cell| *cell.borrow_mut() = Some(flash));
}

/// Travel configuration edited from the web UI.
#[derive(Clone, Copy)]
pub struct Settings {
    pub max_travel_steps: i32,
    pub steps_per_degree_x10: i32,
    pub gearbox_ratio_x10: i32,
    pub max_steps_enabled: bool,
}

impl Settings {
    /// Snapshot the currently active values held in the motor atomics.
    pub fn current() -> Self {
        Self {
            max_travel_steps: motor::MAX_TRAVEL_STEPS.load(Ordering::Relaxed),
            steps_per_degree_x10: motor::STEPS_PER_DEGREE_X10.load(Ordering::Relaxed),
            gearbox_ratio_x10: motor::GEARBOX_RATIO_X10.load(Ordering::Relaxed),
            max_steps_enabled: motor::MAX_STEPS_ENABLED.load(Ordering::Relaxed),
        }
    }

    /// Push the values into the live atomics, clamped to the same safe ranges
    /// the web UI enforces.
    pub fn apply(&self) {
        motor::MAX_TRAVEL_STEPS.store(self.max_travel_steps.clamp(100, 1_000_000), Ordering::Relaxed);
        motor::STEPS_PER_DEGREE_X10.store(self.steps_per_degree_x10.clamp(1, 10_000), Ordering::Relaxed);
        motor::GEARBOX_RATIO_X10.store(self.gearbox_ratio_x10.clamp(1, 1_000), Ordering::Relaxed);
        motor::MAX_STEPS_ENABLED.store(self.max_steps_enabled, Ordering::Relaxed);
    }

    fn encode(&self) -> [u8; RECORD_LEN] {
        let mut raw = [0xFF_u8; RECORD_LEN];
        raw[0..4].copy_from_slice(&MAGIC.to_le_bytes());
        raw[4] = VERSION;
        raw[5] = self.max_steps_enabled as u8;
        raw[8..12].copy_from_slice(&self.max_travel_steps.to_le_bytes());
        raw[12..16].copy_from_slice(&self.steps_per_degree_x10.to_le_bytes());
        raw[16..20].copy_from_slice(&self.gearbox_ratio_x10.to_le_bytes());
        let sum = checksum(&raw[..20]);
        raw[20..24].copy_from_slice(&sum.to_le_bytes());
        raw
    }

    fn decode(raw: &[u8; RECORD_LEN]) -> Option<Self> {
        if u32::from_le_bytes([raw[0], raw[1], raw[2], raw[3]]) != MAGIC || raw[4] != VERSION {
            return None;
        }
        let stored = u32::from_le_bytes([raw[20], raw[21], raw[22], raw[23]]);
        if checksum(&raw[..20]) != stored {
            return None;
        }
        Some(Self {
            max_steps_enabled: raw[5] != 0,
            max_travel_steps: i32::from_le_bytes([raw[8], raw[9], raw[10], raw[11]]),
            steps_per_degree_x10: i32::from_le_bytes([raw[12], raw[13], raw[14], raw[15]]),
            gearbox_ratio_x10: i32::from_le_bytes([raw[16], raw[17], raw[18], raw[19]]),
        })
    }
}

/// Load the stored settings, or `None` if the sector is blank/corrupt.
pub fn load() -> Option<Settings> {
    FLASH.lock(|cell| {
        let mut cell = cell.borrow_mut();
        let flash = cell.as_mut()?;
        let mut raw = [0u8; RECORD_LEN];
        flash.blocking_read(SETTINGS_OFFSET, &mut raw).ok()?;
        Settings::decode(&raw)
    })
}

/// Erase and rewrite the settings sector.
pub fn save(settings: Settings) {
    FLASH.lock(|cell| {
        let mut cell = cell.borrow_mut();
        let Some(flash) = cell.as_mut() else {
            log::warn!("settings not saved: flash driver unavailable");
            return;
        };
        if let Err(e) = flash.blocking_erase(SETTINGS_OFFSET, SETTINGS_OFFSET + ERASE_SIZE as u32) {
            log::error!("settings erase failed: {:?}", e);
            return;
        }
        match flash.blocking_write(SETTINGS_OFFSET, &settings.encode()) {
            Ok(()) => log::info!("settings persisted to flash"),
            Err(e) => log::error!("settings write failed: {:?}", e),
        }
    });
}

fn checksum(bytes: &[u8]) -> u32 {
    let mut hash: u32 = 0x811C_9DC5;
    for &byte in bytes {
        hash ^= byte as u32;
        hash = hash.wrapping_mul(0x0100_0193);
    }
    hash
}
