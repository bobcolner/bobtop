//! Per-box "is this collector's panel currently visible?" gate.
//!
//! Each box (CPU, MEM, DISK, NET, PROC) maps to a single bit in a shared
//! `AtomicU8`. The TUI layout owns the truth and mutates the bitfield when
//! the user shows/hides panels (current trigger: layout preset switch;
//! future: B5 `B`-overlay). Collector tasks read the bit before doing work
//! so a hidden panel costs zero CPU — matching btop's preset model.
//!
//! Lives in `bobtop-core` so both the TUI (which writes) and the
//! collectors (which read) can depend on it without a cycle.

use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::Arc;

/// Identifies a collectible subsystem. Used both as the key for the
/// `BoxesEnabled` bitfield and as the routing tag the daemon hands to
/// `spawn_collector` so each task knows which bit to consult.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Box {
    Cpu,
    Memory,
    Disk,
    Network,
    Process,
}

impl Box {
    const fn bit(self) -> u8 {
        match self {
            Box::Cpu => 1 << 0,
            Box::Memory => 1 << 1,
            Box::Disk => 1 << 2,
            Box::Network => 1 << 3,
            Box::Process => 1 << 4,
        }
    }

    /// Every variant — useful for "enable all" defaults.
    pub const ALL: [Box; 5] = [Box::Cpu, Box::Memory, Box::Disk, Box::Network, Box::Process];
}

/// Cheaply-cloneable handle to the shared per-box enabled bitfield.
/// Reads use `Ordering::Relaxed` — visibility eventually-consistent across
/// tasks is fine, no synchronization with other state required.
#[derive(Debug, Clone)]
pub struct BoxesEnabled(Arc<AtomicU8>);

impl BoxesEnabled {
    /// All boxes enabled (the `Full` layout default).
    pub fn all() -> Self {
        Self::from_bits(Box::ALL.iter().map(|b| b.bit()).fold(0u8, |a, b| a | b))
    }

    /// Build with an explicit set of enabled boxes.
    pub fn with(boxes: &[Box]) -> Self {
        Self::from_bits(boxes.iter().map(|b| b.bit()).fold(0u8, |a, b| a | b))
    }

    fn from_bits(bits: u8) -> Self {
        Self(Arc::new(AtomicU8::new(bits)))
    }

    pub fn is_enabled(&self, b: Box) -> bool {
        self.0.load(Ordering::Relaxed) & b.bit() != 0
    }

    pub fn set(&self, b: Box, on: bool) {
        // Compare-exchange loop: cheaper than a Mutex, and contention is
        // negligible (writes only on layout/preset changes).
        let bit = b.bit();
        let mut cur = self.0.load(Ordering::Relaxed);
        loop {
            let next = if on { cur | bit } else { cur & !bit };
            if cur == next {
                return;
            }
            match self.0.compare_exchange_weak(
                cur,
                next,
                Ordering::Relaxed,
                Ordering::Relaxed,
            ) {
                Ok(_) => return,
                Err(actual) => cur = actual,
            }
        }
    }

    /// Replace the entire bitfield in one shot — used when the layout
    /// preset switches and several boxes flip together.
    pub fn replace(&self, boxes: &[Box]) {
        let bits = boxes.iter().map(|b| b.bit()).fold(0u8, |a, b| a | b);
        self.0.store(bits, Ordering::Relaxed);
    }
}

impl Default for BoxesEnabled {
    fn default() -> Self {
        Self::all()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn all_starts_enabled() {
        let e = BoxesEnabled::all();
        for b in Box::ALL {
            assert!(e.is_enabled(b), "{b:?} should start enabled");
        }
    }

    #[test]
    fn with_only_named_set() {
        let e = BoxesEnabled::with(&[Box::Cpu, Box::Process]);
        assert!(e.is_enabled(Box::Cpu));
        assert!(e.is_enabled(Box::Process));
        assert!(!e.is_enabled(Box::Memory));
        assert!(!e.is_enabled(Box::Disk));
        assert!(!e.is_enabled(Box::Network));
    }

    #[test]
    fn set_toggles_individual_bits() {
        let e = BoxesEnabled::all();
        e.set(Box::Network, false);
        assert!(!e.is_enabled(Box::Network));
        assert!(e.is_enabled(Box::Cpu));
        e.set(Box::Network, true);
        assert!(e.is_enabled(Box::Network));
    }

    #[test]
    fn replace_overwrites_whole_field() {
        let e = BoxesEnabled::all();
        e.replace(&[Box::Memory]);
        assert!(e.is_enabled(Box::Memory));
        assert!(!e.is_enabled(Box::Cpu));
        assert!(!e.is_enabled(Box::Process));
    }

    #[test]
    fn cloned_handle_shares_state() {
        let a = BoxesEnabled::all();
        let b = a.clone();
        a.set(Box::Disk, false);
        assert!(!b.is_enabled(Box::Disk));
    }
}
