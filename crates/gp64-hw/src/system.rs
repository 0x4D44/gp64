//! System Control Port A (Port 0x92)
//!
//! Port 0x92 is the "System Control Port A" found on AT and later systems.
//! It provides:
//! - Bit 0: Fast CPU reset (write-only, pulse triggers reset)
//! - Bit 1: A20 gate control (read/write)
//!
//! This provides an alternative to the slower 8042 keyboard controller
//! method of controlling A20.

/// Guest-visible source that requested a system reset.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GuestResetSource {
    /// CPU triple fault reset.
    CpuTripleFault,
    /// Fast reset pulse via System Control Port A bit 0.
    Port92FastReset,
    /// 8042 controller command 0xFE pulsed the output-port reset line.
    KeyboardControllerPulseOutputPort,
}

/// Action requested by system control port write
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SystemControlAction {
    /// Fast CPU reset requested (bit 0 pulsed high)
    Reset(GuestResetSource),
    /// A20 gate state changed
    A20Changed(bool),
}

/// System Control Port A (0x92) state
#[derive(Clone)]
pub struct SystemControl {
    /// Port 0x92 value (only bit 1 is stored; bit 0 is write-only pulse)
    port_92: u8,
    /// Pending action from last write (consumed by emulator)
    pending_action: Option<SystemControlAction>,
}

impl Default for SystemControl {
    fn default() -> Self {
        Self::new()
    }
}

impl SystemControl {
    pub fn new() -> Self {
        Self {
            // A20 starts enabled to match Memory module default (a20_enabled: true)
            port_92: 0x02,
            pending_action: None,
        }
    }

    /// Read port 0x92 value
    pub fn read_port_92(&self) -> u8 {
        self.port_92
    }

    /// Write to port 0x92
    ///
    /// Bit 0: Fast reset (write-only pulse) - if set, triggers CPU reset
    /// Bit 1: A20 gate - 0 = disabled, 1 = enabled
    ///
    /// If bit 0 is set, reset takes priority (A20 change is ignored since
    /// system will reset anyway).
    pub fn write_port_92(&mut self, value: u8) {
        let old = self.port_92;

        // Fast reset: pulse on bit 0 (write-only, don't store)
        // Check this FIRST - if resetting, A20 doesn't matter
        if value & 0x01 != 0 {
            self.pending_action = Some(SystemControlAction::Reset(
                GuestResetSource::Port92FastReset,
            ));
            return;
        }

        // Store value without bit 0 (bit 0 is write-only pulse)
        self.port_92 = value & 0xFE;

        // A20 gate changed?
        if (old ^ value) & 0x02 != 0 {
            self.pending_action = Some(SystemControlAction::A20Changed(value & 0x02 != 0));
        }
    }

    /// Get A20 gate state from port 0x92
    pub fn a20_enabled(&self) -> bool {
        self.port_92 & 0x02 != 0
    }

    /// Set the port 0x92 A20 source latch directly.
    pub fn set_a20(&mut self, enabled: bool) {
        if enabled {
            self.port_92 |= 0x02;
        } else {
            self.port_92 &= !0x02;
        }
    }

    /// Take pending action (returns None if no action pending)
    pub fn take_pending_action(&mut self) -> Option<SystemControlAction> {
        self.pending_action.take()
    }

    /// Return whether a port 0x92 reset/A20 side effect is waiting for the
    /// outer execution loop to consume it.
    pub fn has_pending_action(&self) -> bool {
        self.pending_action.is_some()
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::*;

    #[test]
    fn test_system_control_default() {
        let sys = SystemControl::new();
        assert_eq!(sys.read_port_92(), 0x02);
        assert!(sys.a20_enabled());
    }

    #[test]
    fn test_port_92_a20_disable() {
        let mut sys = SystemControl::new();

        // Default is A20 enabled; disable it
        sys.write_port_92(0x00);

        assert_eq!(sys.read_port_92(), 0x00);
        assert!(!sys.a20_enabled());
        assert_eq!(
            sys.take_pending_action(),
            Some(SystemControlAction::A20Changed(false))
        );
        assert_eq!(sys.take_pending_action(), None); // Consumed
    }

    #[test]
    fn test_port_92_a20_reenable() {
        let mut sys = SystemControl::new();

        // Disable then re-enable A20
        sys.write_port_92(0x00);
        let _ = sys.take_pending_action();

        sys.write_port_92(0x02);

        assert_eq!(sys.read_port_92(), 0x02);
        assert!(sys.a20_enabled());
        assert_eq!(
            sys.take_pending_action(),
            Some(SystemControlAction::A20Changed(true))
        );
    }

    #[test]
    fn test_port_92_no_change_no_action() {
        let mut sys = SystemControl::new();

        // Write same value (A20 enabled, matching default)
        sys.write_port_92(0x02);

        // No action should be pending
        assert_eq!(sys.take_pending_action(), None);
    }

    #[test]
    fn test_port_92_reset_takes_priority() {
        let mut sys = SystemControl::new();

        // Write both reset and A20 disable (bit 0 = reset, bit 1 absent = A20 off)
        sys.write_port_92(0x01);

        // Reset should take priority
        assert_eq!(
            sys.take_pending_action(),
            Some(SystemControlAction::Reset(
                GuestResetSource::Port92FastReset
            ))
        );
        // A20 should still be enabled (reset short-circuits, port_92 unchanged)
        assert!(sys.a20_enabled());
    }

    #[test]
    fn test_cpu_triple_fault_reset_source_is_distinct() {
        assert_ne!(
            GuestResetSource::CpuTripleFault,
            GuestResetSource::Port92FastReset
        );
        assert_ne!(
            GuestResetSource::CpuTripleFault,
            GuestResetSource::KeyboardControllerPulseOutputPort
        );
    }

    #[test]
    fn test_port_92_bit0_not_stored() {
        let mut sys = SystemControl::new();

        // Disable then re-enable A20
        sys.write_port_92(0x00);
        let _ = sys.take_pending_action();
        sys.write_port_92(0x02);
        let _ = sys.take_pending_action();

        // Reading back should not include bit 0
        assert_eq!(sys.read_port_92(), 0x02);
    }

    #[test]
    fn test_port_92_set_a20_direct_latch() {
        let mut sys = SystemControl::new();

        sys.set_a20(true);
        assert!(sys.a20_enabled());
        assert_eq!(sys.read_port_92(), 0x02);

        sys.set_a20(false);
        assert!(!sys.a20_enabled());
        assert_eq!(sys.read_port_92(), 0x00);
    }

    #[test]
    fn test_set_a20_does_not_set_pending_action() {
        let mut sys = SystemControl::new();
        sys.set_a20(true);
        assert_eq!(sys.take_pending_action(), None);
        sys.set_a20(false);
        assert_eq!(sys.take_pending_action(), None);
    }

    #[test]
    fn test_port_92_higher_bits_preserved() {
        let mut sys = SystemControl::new();

        // Write with higher bits set
        sys.write_port_92(0xFE); // All bits except bit 0

        // Should store value (except bit 0 which is already 0)
        assert_eq!(sys.read_port_92(), 0xFE);
    }
}
