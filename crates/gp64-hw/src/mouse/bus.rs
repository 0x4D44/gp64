//! Bus mouse emulation
//!
//! Supports two bus mouse interfaces:
//! - Microsoft InPort mouse: I/O ports 0x23C-0x23F
//! - Microsoft Bus Mouse: I/O ports 0x23C-0x23F (same as InPort, slightly different behavior)
//!
//! Bus mice connect directly to the ISA bus via a dedicated adapter card,
//! unlike serial mice which use a COM port.

/// Bus mouse type
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BusMouseType {
    /// Microsoft InPort mouse
    InPort,
    /// Microsoft Bus Mouse (older protocol)
    BusMouse,
}

/// InPort register indices (selected via address register)
mod inport_regs {
    /// Status register (read-only)
    pub const STATUS: u8 = 0x00;
    /// Data register 1 - buttons and X movement
    pub const DATA1: u8 = 0x01;
    /// Data register 2 - Y movement
    pub const DATA2: u8 = 0x02;
    /// Mode register (write-only when selected as register)
    pub const MODE: u8 = 0x07;
}

/// InPort status register bits
mod inport_status {
    /// Button 3 (middle) status (active low)
    pub const BUTTON3: u8 = 0x01;
    /// Button 2 (right) status (active low)
    pub const BUTTON2: u8 = 0x02;
    /// Button 1 (left) status (active low)
    pub const BUTTON1: u8 = 0x04;
    /// Movement occurred
    pub const MOVEMENT: u8 = 0x40;
}

/// InPort mode register bits
#[allow(dead_code)] // Hardware register definitions for completeness
mod inport_mode {
    /// Enable data hold mode (data latched on read)
    pub const HOLD: u8 = 0x20;
    /// Enable timer interrupts
    pub const TIMER_INT: u8 = 0x10;
    /// Enable button interrupts
    pub const BUTTON_INT: u8 = 0x08;
    /// Enable movement interrupts
    pub const MOVEMENT_INT: u8 = 0x04;
    /// Hz rate select bit 1
    pub const HZ_1: u8 = 0x02;
    /// Hz rate select bit 0
    pub const HZ_0: u8 = 0x01;
}

/// Bus mouse state
#[derive(Debug, Clone)]
pub struct BusMouse {
    /// Mouse type
    mouse_type: BusMouseType,
    /// Current address register (for InPort)
    address_reg: u8,
    /// Mode register value
    mode: u8,
    /// Accumulated X movement
    delta_x: i32,
    /// Accumulated Y movement
    delta_y: i32,
    /// Button state (bit 0 = left, bit 1 = right, bit 2 = middle)
    buttons: u8,
    /// Last buttons state (for change detection)
    last_buttons: u8,
    /// Latched X movement for current read cycle
    latched_x: i8,
    /// Latched Y movement for current read cycle
    latched_y: i8,
    /// Latched buttons for current read cycle
    latched_buttons: u8,
    /// Data has been latched (hold mode)
    data_latched: bool,
    /// Interrupt pending flag
    interrupt_pending: bool,
    /// Control register (Bus Mouse)
    control: u8,
}

impl Default for BusMouse {
    fn default() -> Self {
        Self::new(BusMouseType::InPort)
    }
}

impl BusMouse {
    /// Create a new bus mouse
    pub fn new(mouse_type: BusMouseType) -> Self {
        Self {
            mouse_type,
            address_reg: 0,
            mode: 0,
            delta_x: 0,
            delta_y: 0,
            buttons: 0,
            last_buttons: 0,
            latched_x: 0,
            latched_y: 0,
            latched_buttons: 0,
            data_latched: false,
            interrupt_pending: false,
            control: 0,
        }
    }

    /// Get the mouse type
    pub fn mouse_type(&self) -> BusMouseType {
        self.mouse_type
    }

    /// Set the mouse type
    pub fn set_mouse_type(&mut self, mouse_type: BusMouseType) {
        self.mouse_type = mouse_type;
    }

    /// Update mouse movement (delta values)
    pub fn update_movement(&mut self, dx: i32, dy: i32) {
        self.delta_x = self.delta_x.saturating_add(dx);
        self.delta_y = self.delta_y.saturating_add(dy);

        // Check for movement interrupt
        if (self.mode & inport_mode::MOVEMENT_INT) != 0 && (dx != 0 || dy != 0) {
            self.interrupt_pending = true;
        }
    }

    /// Set button states
    pub fn set_buttons(&mut self, left: bool, right: bool, middle: bool) {
        self.last_buttons = self.buttons;
        self.buttons = 0;
        if left {
            self.buttons |= 0x01;
        }
        if right {
            self.buttons |= 0x02;
        }
        if middle {
            self.buttons |= 0x04;
        }

        // Check for button interrupt
        if (self.mode & inport_mode::BUTTON_INT) != 0 && self.buttons != self.last_buttons {
            self.interrupt_pending = true;
        }
    }

    /// Check if an interrupt is pending
    pub fn interrupt_pending(&self) -> bool {
        self.interrupt_pending
    }

    /// Clear the interrupt pending flag
    pub fn clear_interrupt(&mut self) {
        self.interrupt_pending = false;
    }

    /// Latch current data for reading (hold mode)
    fn latch_data(&mut self) {
        if self.data_latched {
            return;
        }

        // Clamp movement to -128..127
        self.latched_x = self.delta_x.clamp(-128, 127) as i8;
        self.latched_y = self.delta_y.clamp(-128, 127) as i8;
        self.latched_buttons = self.buttons;

        // Subtract latched values from accumulators
        self.delta_x -= self.latched_x as i32;
        self.delta_y -= self.latched_y as i32;

        self.data_latched = true;
    }

    /// Release latched data
    fn release_latch(&mut self) {
        self.data_latched = false;
    }

    /// Read from I/O port
    pub fn read(&mut self, port: u16) -> u8 {
        let offset = (port & 0x03) as u8;

        match self.mouse_type {
            BusMouseType::InPort => self.read_inport(offset),
            BusMouseType::BusMouse => self.read_busmouse(offset),
        }
    }

    /// Write to I/O port
    pub fn write(&mut self, port: u16, value: u8) {
        let offset = (port & 0x03) as u8;

        match self.mouse_type {
            BusMouseType::InPort => self.write_inport(offset, value),
            BusMouseType::BusMouse => self.write_busmouse(offset, value),
        }
    }

    /// Read InPort register
    fn read_inport(&mut self, offset: u8) -> u8 {
        match offset {
            // Address register
            0 => self.address_reg,
            // Data register
            1 => self.read_inport_data(),
            // Identification register (always 0xDE for InPort)
            2 => 0xDE,
            // Reserved
            3 => 0xFF,
            _ => 0xFF,
        }
    }

    /// Write InPort register
    fn write_inport(&mut self, offset: u8, value: u8) {
        match offset {
            // Address register
            0 => {
                // If hold mode is enabled and we're switching away from status,
                // latch the data
                if (self.mode & inport_mode::HOLD) != 0 && self.address_reg == inport_regs::STATUS {
                    self.latch_data();
                }
                self.address_reg = value & 0x07;
            }
            // Data register
            1 if self.address_reg == inport_regs::MODE => {
                self.mode = value;
                // If hold mode is disabled, release latch
                if (value & inport_mode::HOLD) == 0 {
                    self.release_latch();
                }
            }
            // Identification register (read-only)
            2 => {}
            // Reserved
            3 => {}
            _ => {}
        }
    }

    /// Read InPort data register based on address register selection
    fn read_inport_data(&mut self) -> u8 {
        match self.address_reg {
            inport_regs::STATUS => {
                // If hold mode enabled, latch on status read
                if (self.mode & inport_mode::HOLD) != 0 {
                    self.latch_data();
                }

                let btns = if self.data_latched {
                    self.latched_buttons
                } else {
                    self.buttons
                };

                // Status register: buttons are active low
                let mut status = 0u8;
                if (btns & 0x04) == 0 {
                    status |= inport_status::BUTTON3;
                } // Middle
                if (btns & 0x02) == 0 {
                    status |= inport_status::BUTTON2;
                } // Right
                if (btns & 0x01) == 0 {
                    status |= inport_status::BUTTON1;
                } // Left

                // Movement flag
                if self.delta_x != 0 || self.delta_y != 0 {
                    status |= inport_status::MOVEMENT;
                }

                status
            }
            inport_regs::DATA1 => {
                // X movement + button bits
                let x = if self.data_latched {
                    self.latched_x
                } else {
                    let x = self.delta_x.clamp(-128, 127) as i8;
                    self.delta_x -= x as i32;
                    x
                };

                let btns = if self.data_latched {
                    self.latched_buttons
                } else {
                    self.buttons
                };

                // Lower 6 bits: X movement, upper 2 bits: buttons
                let btn_bits = if (btns & 0x01) != 0 { 0x00 } else { 0x40 } // Left (active low)
                             | if (btns & 0x02) != 0 { 0x00 } else { 0x80 }; // Right (active low)
                (x as u8 & 0x3F) | btn_bits
            }
            inport_regs::DATA2 => {
                // Y movement
                let y = if self.data_latched {
                    // Release latch after reading Y
                    let y = self.latched_y;
                    self.release_latch();
                    y
                } else {
                    let y = self.delta_y.clamp(-128, 127) as i8;
                    self.delta_y -= y as i32;
                    y
                };
                y as u8
            }
            inport_regs::MODE => self.mode,
            _ => 0xFF,
        }
    }

    /// Read Microsoft Bus Mouse register
    fn read_busmouse(&mut self, offset: u8) -> u8 {
        match offset {
            // Data register
            0 => {
                // Get appropriate movement based on control register
                match self.control & 0x07 {
                    0x00 => {
                        // X low nibble
                        (self.delta_x.clamp(-128, 127) as u8) & 0x0F
                    }
                    0x01 => {
                        // X high nibble
                        ((self.delta_x.clamp(-128, 127) as u8) >> 4) & 0x0F
                    }
                    0x02 => {
                        // Y low nibble
                        (self.delta_y.clamp(-128, 127) as u8) & 0x0F
                    }
                    0x03 => {
                        // Y high nibble, then clear movement
                        let result = ((self.delta_y.clamp(-128, 127) as u8) >> 4) & 0x0F;
                        // Clear deltas after full read cycle
                        self.delta_x = 0;
                        self.delta_y = 0;
                        result
                    }
                    _ => 0,
                }
            }
            // Signature register (identifies as bus mouse)
            1 => 0xA5,
            // Control register
            2 => self.control,
            // Interrupt status
            3 => {
                // Buttons (active low)
                let mut result = 0u8;
                if (self.buttons & 0x01) == 0 {
                    result |= 0x04;
                } // Left
                if (self.buttons & 0x02) == 0 {
                    result |= 0x01;
                } // Right
                if (self.buttons & 0x04) == 0 {
                    result |= 0x02;
                } // Middle
                result
            }
            _ => 0xFF,
        }
    }

    /// Write Microsoft Bus Mouse register
    fn write_busmouse(&mut self, offset: u8, value: u8) {
        match offset {
            // Data register (read-only)
            0 => {}
            // Signature register (read-only)
            1 => {}
            // Control register
            2 => {
                self.control = value;
                // Control bits:
                // Bits 0-2: Data port read select
                // Bit 3: Interrupt enable
                // Bit 4: Counter hold
                // Bit 5: Timer interrupt enable
                if (value & 0x80) != 0 {
                    // Reset mouse
                    self.delta_x = 0;
                    self.delta_y = 0;
                    self.interrupt_pending = false;
                }
            }
            // Configuration register
            3 => {
                // Clear interrupt on write
                self.interrupt_pending = false;
            }
            _ => {}
        }
    }

    /// Reset the mouse state
    pub fn reset(&mut self) {
        self.address_reg = 0;
        self.mode = 0;
        self.delta_x = 0;
        self.delta_y = 0;
        self.buttons = 0;
        self.last_buttons = 0;
        self.latched_x = 0;
        self.latched_y = 0;
        self.latched_buttons = 0;
        self.data_latched = false;
        self.interrupt_pending = false;
        self.control = 0;
    }

    /// Get current button state
    pub fn button_state(&self) -> u8 {
        self.buttons
    }

    /// Get accumulated movement
    pub fn movement(&self) -> (i32, i32) {
        (self.delta_x, self.delta_y)
    }

    /// Get the base I/O port for this mouse
    pub fn base_port() -> u16 {
        0x23C
    }

    /// Check if a port is handled by the bus mouse
    pub fn handles_port(port: u16) -> bool {
        (0x23C..=0x23F).contains(&port)
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::*;

    fn sign_extend_6bit(value: u8) -> i8 {
        let value = (value & 0x3F) as i8;
        if value > 31 {
            value - 64
        } else {
            value
        }
    }

    fn bb<T>(value: T) -> T {
        std::hint::black_box(value)
    }

    #[test]
    fn test_bus_mouse_creation() {
        let mouse = BusMouse::new(BusMouseType::InPort);
        assert_eq!(mouse.mouse_type(), BusMouseType::InPort);
        assert_eq!(mouse.button_state(), 0);
        assert_eq!(mouse.movement(), (0, 0));
    }

    #[test]
    fn test_inport_identification() {
        let mut mouse = BusMouse::new(BusMouseType::InPort);

        // Port 23E (offset 2) returns InPort ID 0xDE
        let id = mouse.read(0x23E);
        assert_eq!(id, 0xDE, "InPort identification byte");
    }

    #[test]
    fn test_busmouse_identification() {
        let mut mouse = BusMouse::new(BusMouseType::BusMouse);

        // Port 23D (offset 1) returns Bus Mouse signature 0xA5
        let sig = mouse.read(0x23D);
        assert_eq!(sig, 0xA5, "Bus Mouse signature byte");
    }

    #[test]
    fn test_inport_movement() {
        let mut mouse = BusMouse::new(BusMouseType::InPort);

        mouse.update_movement(10, -5);

        // Select DATA2 register for Y movement
        mouse.write(0x23C, inport_regs::DATA2);
        let y = mouse.read(0x23D) as i8;
        assert_eq!(y, -5, "Y movement");

        // Select DATA1 register for X movement
        mouse.write(0x23C, inport_regs::DATA1);
        let data1 = mouse.read(0x23D);
        let x = sign_extend_6bit(data1);
        assert_eq!(x, 10, "X movement");

        mouse.update_movement(-10, 0);
        mouse.write(0x23C, inport_regs::DATA1);
        let data1 = mouse.read(0x23D);
        let x = sign_extend_6bit(data1);
        assert_eq!(x, -10, "Negative X movement");
    }

    #[test]
    fn test_inport_status_movement_flag_x_only() {
        let mut mouse = BusMouse::new(BusMouseType::InPort);

        mouse.update_movement(5, 0);

        mouse.write(0x23C, inport_regs::STATUS);
        let status = mouse.read(0x23D);
        assert_ne!(status & inport_status::MOVEMENT, 0, "Movement flag set");
    }

    #[test]
    fn test_inport_status_movement_flag_y_only() {
        let mut mouse = BusMouse::new(BusMouseType::InPort);

        mouse.update_movement(0, 5);

        mouse.write(0x23C, inport_regs::STATUS);
        let status = mouse.read(0x23D);
        assert_ne!(status & inport_status::MOVEMENT, 0, "Movement flag set");
    }

    #[test]
    fn test_inport_data1_latched_buttons_and_x() {
        let mut mouse = BusMouse::new(BusMouseType::InPort);

        mouse.write(0x23C, inport_regs::MODE);
        mouse.write(0x23D, inport_mode::HOLD);

        mouse.set_buttons(true, true, false);
        mouse.update_movement(12, 0);

        mouse.write(0x23C, inport_regs::STATUS);
        let _ = mouse.read(0x23D);

        mouse.update_movement(50, 0);

        mouse.write(0x23C, inport_regs::DATA1);
        let data1 = mouse.read(0x23D);
        let x = sign_extend_6bit(data1);
        assert_eq!(x, 12, "Latched X value");
        assert_eq!(data1 & 0xC0, 0x00, "Buttons pressed set bits low");
    }

    #[test]
    fn test_inport_buttons() {
        let mut mouse = BusMouse::new(BusMouseType::InPort);

        mouse.set_buttons(true, false, true); // Left + Middle

        // Select STATUS register
        mouse.write(0x23C, inport_regs::STATUS);
        let status = mouse.read(0x23D);

        // Buttons are active low
        assert_eq!(
            status & inport_status::BUTTON1,
            0,
            "Left pressed (active low)"
        );
        assert_ne!(
            status & inport_status::BUTTON2,
            0,
            "Right not pressed (active low)"
        );
        assert_eq!(
            status & inport_status::BUTTON3,
            0,
            "Middle pressed (active low)"
        );
    }

    #[test]
    fn test_inport_hold_mode() {
        let mut mouse = BusMouse::new(BusMouseType::InPort);

        // Enable hold mode
        mouse.write(0x23C, inport_regs::MODE);
        mouse.write(0x23D, inport_mode::HOLD);

        // Add movement
        mouse.update_movement(20, 15);

        // Select STATUS to trigger latch
        mouse.write(0x23C, inport_regs::STATUS);
        let _ = mouse.read(0x23D);

        // Add more movement (shouldn't affect latched data)
        mouse.update_movement(50, 50);

        // Read latched Y (and release latch)
        mouse.write(0x23C, inport_regs::DATA2);
        let y = mouse.read(0x23D) as i8;
        assert_eq!(y, 15, "Latched Y value");

        // Now should see new movement
        mouse.write(0x23C, inport_regs::DATA2);
        let y2 = mouse.read(0x23D) as i8;
        assert_eq!(y2, 50, "New Y value after latch release");
    }

    #[test]
    fn test_inport_write_data_ignores_non_mode_address() {
        let mut mouse = BusMouse::new(BusMouseType::InPort);
        mouse.mode = inport_mode::MOVEMENT_INT;

        mouse.write(0x23C, bb(inport_regs::STATUS));
        mouse.write(0x23D, bb(0xFF));

        assert_eq!(mouse.mode, inport_mode::MOVEMENT_INT);
    }

    #[test]
    fn test_busmouse_movement_sequence() {
        let mut mouse = BusMouse::new(BusMouseType::BusMouse);

        mouse.update_movement(0x12, 0x34);

        // Read X low nibble (control = 0)
        mouse.write(0x23E, 0x00);
        let x_lo = mouse.read(0x23C);
        assert_eq!(x_lo, 0x02, "X low nibble");

        // Read X high nibble (control = 1)
        mouse.write(0x23E, 0x01);
        let x_hi = mouse.read(0x23C);
        assert_eq!(x_hi, 0x01, "X high nibble");

        // Read Y low nibble (control = 2)
        mouse.write(0x23E, 0x02);
        let y_lo = mouse.read(0x23C);
        assert_eq!(y_lo, 0x04, "Y low nibble");

        // Read Y high nibble (control = 3) - this clears movement
        mouse.write(0x23E, 0x03);
        let y_hi = mouse.read(0x23C);
        assert_eq!(y_hi, 0x03, "Y high nibble");

        // Movement should be cleared
        assert_eq!(mouse.movement(), (0, 0), "Movement cleared");
    }

    #[test]
    fn test_busmouse_buttons() {
        let mut mouse = BusMouse::new(BusMouseType::BusMouse);

        mouse.set_buttons(bb(true), bb(true), bb(false)); // Left + Right

        // Read button status from port 23F
        let btns = mouse.read(0x23F);

        // Buttons are active low
        assert_eq!(btns & 0x04, 0, "Left pressed");
        assert_eq!(btns & 0x01, 0, "Right pressed");
        assert_ne!(btns & 0x02, 0, "Middle not pressed");
    }

    #[test]
    fn test_busmouse_buttons_middle_pressed() {
        let mut mouse = BusMouse::new(BusMouseType::BusMouse);

        mouse.set_buttons(bb(false), bb(false), bb(true)); // Middle

        let btns = mouse.read(0x23F);
        assert_eq!(btns & 0x02, 0, "Middle pressed");
    }

    #[test]
    fn test_busmouse_buttons_none_pressed() {
        let mut mouse = BusMouse::new(BusMouseType::BusMouse);

        mouse.set_buttons(false, false, false);

        let btns = mouse.read(0x23F);
        assert_ne!(btns & 0x04, 0, "Left not pressed");
        assert_ne!(btns & 0x01, 0, "Right not pressed");
        assert_ne!(btns & 0x02, 0, "Middle not pressed");
    }

    #[test]
    fn test_movement_accumulation() {
        let mut mouse = BusMouse::new(BusMouseType::InPort);

        mouse.update_movement(10, 5);
        mouse.update_movement(20, 15);
        mouse.update_movement(-5, -3);

        assert_eq!(mouse.movement(), (25, 17));
    }

    #[test]
    fn test_movement_clamping() {
        let mut mouse = BusMouse::new(BusMouseType::InPort);

        mouse.update_movement(200, -200);

        // Select DATA2 for Y
        mouse.write(0x23C, inport_regs::DATA2);
        let y = mouse.read(0x23D) as i8;
        assert_eq!(y, -128, "Y clamped to -128");

        // Remaining movement should still be in accumulator
        assert_eq!(mouse.delta_y, -72, "Remaining Y movement");
    }

    #[test]
    fn test_movement_interrupt() {
        let mut mouse = BusMouse::new(BusMouseType::InPort);

        // Enable movement interrupt
        mouse.write(0x23C, inport_regs::MODE);
        mouse.write(0x23D, inport_mode::MOVEMENT_INT);

        assert!(!mouse.interrupt_pending());

        mouse.update_movement(1, 0);

        assert!(mouse.interrupt_pending());
    }

    #[test]
    fn test_movement_interrupt_y_only() {
        let mut mouse = BusMouse::new(BusMouseType::InPort);

        mouse.write(0x23C, inport_regs::MODE);
        mouse.write(0x23D, inport_mode::MOVEMENT_INT);

        mouse.update_movement(bb(0), bb(1));

        assert!(mouse.interrupt_pending());
    }

    #[test]
    fn test_movement_interrupt_no_movement() {
        let mut mouse = BusMouse::new(BusMouseType::InPort);

        mouse.write(0x23C, inport_regs::MODE);
        mouse.write(0x23D, inport_mode::MOVEMENT_INT);

        mouse.update_movement(bb(0), bb(0));

        assert!(!mouse.interrupt_pending());
    }

    #[test]
    fn test_button_interrupt() {
        let mut mouse = BusMouse::new(BusMouseType::InPort);

        // Enable button interrupt
        mouse.write(0x23C, inport_regs::MODE);
        mouse.write(0x23D, inport_mode::BUTTON_INT);

        assert!(!mouse.interrupt_pending());

        mouse.set_buttons(bb(true), bb(false), bb(false));

        assert!(mouse.interrupt_pending());
    }

    #[test]
    fn test_button_interrupt_no_change() {
        let mut mouse = BusMouse::new(BusMouseType::InPort);

        mouse.write(0x23C, inport_regs::MODE);
        mouse.write(0x23D, inport_mode::BUTTON_INT);

        mouse.set_buttons(bb(false), bb(false), bb(false));

        assert!(!mouse.interrupt_pending());
    }

    #[test]
    fn test_reset() {
        let mut mouse = BusMouse::new(BusMouseType::InPort);

        mouse.update_movement(100, 100);
        mouse.set_buttons(true, true, true);
        mouse.write(0x23C, inport_regs::MODE);
        mouse.write(0x23D, 0xFF);

        mouse.reset();

        assert_eq!(mouse.movement(), (0, 0));
        assert_eq!(mouse.button_state(), 0);
        assert_eq!(mouse.mode, 0);
        assert!(!mouse.interrupt_pending());
    }

    #[test]
    fn test_handles_port() {
        assert!(BusMouse::handles_port(0x23C));
        assert!(BusMouse::handles_port(0x23D));
        assert!(BusMouse::handles_port(0x23E));
        assert!(BusMouse::handles_port(0x23F));
        assert!(!BusMouse::handles_port(0x23B));
        assert!(!BusMouse::handles_port(0x240));
    }

    #[test]
    fn test_base_port() {
        assert_eq!(BusMouse::base_port(), 0x23C);
    }

    #[test]
    fn test_busmouse_reset_via_control() {
        let mut mouse = BusMouse::new(BusMouseType::BusMouse);

        mouse.update_movement(50, 50);

        // Write 0x80 to control register to reset
        mouse.write(0x23E, 0x80);

        assert_eq!(mouse.movement(), (0, 0));
    }

    #[test]
    fn test_clear_interrupt() {
        let mut mouse = BusMouse::new(BusMouseType::InPort);

        // Enable movement interrupt
        mouse.write(0x23C, inport_regs::MODE);
        mouse.write(0x23D, inport_mode::MOVEMENT_INT);

        mouse.update_movement(1, 1);
        assert!(mouse.interrupt_pending());

        mouse.clear_interrupt();
        assert!(!mouse.interrupt_pending());
    }

    #[test]
    fn test_set_mouse_type() {
        let mut mouse = BusMouse::new(BusMouseType::InPort);
        assert_eq!(mouse.mouse_type(), BusMouseType::InPort);

        mouse.set_mouse_type(BusMouseType::BusMouse);
        assert_eq!(mouse.mouse_type(), BusMouseType::BusMouse);
    }
}
