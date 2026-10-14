//! Serial mouse emulation
//!
//! Supports multiple serial mouse protocols:
//! - Microsoft Mouse: 3-byte packets (2 buttons)
//! - Microsoft Mouse with Logitech extension: 3-4 byte packets (3 buttons)
//! - MouseSystems: 5-byte packets (3 buttons)
//!
//! Standard serial mouse settings: 1200 baud, 7N1 (7 data bits, no parity, 1 stop bit)

use gp64_types::FixedRing;

/// Serial mouse protocol types
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SerialMouseProtocol {
    /// Microsoft Mouse protocol (2 buttons, 3-byte packets)
    Microsoft,
    /// Microsoft Mouse with Logitech extension (3 buttons, 3-4 byte packets)
    MicrosoftLogitech,
    /// MouseSystems protocol (3 buttons, 5-byte packets)
    MouseSystems,
}

/// Serial mouse state
#[derive(Debug, Clone)]
pub struct SerialMouse {
    /// Mouse protocol in use
    protocol: SerialMouseProtocol,
    /// Current X position (accumulated delta)
    delta_x: i32,
    /// Current Y position (accumulated delta)
    delta_y: i32,
    /// Left button state
    left_button: bool,
    /// Right button state
    right_button: bool,
    /// Middle button state (3-button mice only)
    middle_button: bool,
    /// Output packet queue
    packet_queue: FixedRing<u8, 16>,
    /// RTS state (mouse enabled when low-to-high transition)
    rts_active: bool,
    /// DTR state (mouse powered when DTR is high)
    dtr_active: bool,
    /// Last reported button state (for change detection)
    last_left: bool,
    last_right: bool,
    last_middle: bool,
    /// Accumulated movement since last packet
    accum_x: i32,
    accum_y: i32,
    /// Whether mouse has been identified (sent ID string)
    identified: bool,
    /// Simulation time accumulator for baud rate timing
    byte_timer: f64,
}

impl Default for SerialMouse {
    fn default() -> Self {
        Self::new(SerialMouseProtocol::Microsoft)
    }
}

impl SerialMouse {
    /// Create a new serial mouse with the specified protocol
    pub fn new(protocol: SerialMouseProtocol) -> Self {
        Self {
            protocol,
            delta_x: 0,
            delta_y: 0,
            left_button: false,
            right_button: false,
            middle_button: false,
            packet_queue: FixedRing::new(),
            rts_active: false,
            dtr_active: false,
            last_left: false,
            last_right: false,
            last_middle: false,
            accum_x: 0,
            accum_y: 0,
            identified: false,
            byte_timer: 0.0,
        }
    }

    /// Get the current protocol
    pub fn protocol(&self) -> SerialMouseProtocol {
        self.protocol
    }

    /// Set the mouse protocol
    pub fn set_protocol(&mut self, protocol: SerialMouseProtocol) {
        self.protocol = protocol;
        self.packet_queue.clear();
        self.identified = false;
    }

    /// Update mouse movement (delta values)
    pub fn update_movement(&mut self, dx: i32, dy: i32) {
        self.accum_x += dx;
        self.accum_y += dy;
    }

    /// Set button state
    pub fn set_buttons(&mut self, left: bool, right: bool, middle: bool) {
        self.left_button = left;
        self.right_button = right;
        self.middle_button = middle;
    }

    /// Set left button state
    pub fn set_left_button(&mut self, pressed: bool) {
        self.left_button = pressed;
    }

    /// Set right button state
    pub fn set_right_button(&mut self, pressed: bool) {
        self.right_button = pressed;
    }

    /// Set middle button state
    pub fn set_middle_button(&mut self, pressed: bool) {
        self.middle_button = pressed;
    }

    /// Handle RTS (Request To Send) signal change from serial port
    /// Mouse sends identification on low-to-high RTS transition
    pub fn set_rts(&mut self, active: bool) {
        let was_active = self.rts_active;
        self.rts_active = active;

        // Send mouse identification on RTS low-to-high transition (mouse reset)
        if !was_active && active && self.dtr_active {
            self.send_identification();
        }
    }

    /// Handle DTR (Data Terminal Ready) signal change
    /// Mouse is powered when DTR is high
    pub fn set_dtr(&mut self, active: bool) {
        let was_active = self.dtr_active;
        self.dtr_active = active;

        if !was_active && active {
            // Power on - reset state
            self.packet_queue.clear();
            self.identified = false;
            self.accum_x = 0;
            self.accum_y = 0;
            self.last_left = false;
            self.last_right = false;
            self.last_middle = false;
        }
    }

    /// Send mouse identification string
    fn send_identification(&mut self) {
        self.packet_queue.clear();
        self.identified = true;

        match self.protocol {
            SerialMouseProtocol::Microsoft => {
                // Microsoft Mouse sends 'M' on reset
                self.packet_queue.push_back(b'M');
            }
            SerialMouseProtocol::MicrosoftLogitech => {
                // Logitech 3-button sends 'M3' on reset
                self.packet_queue.push_back(b'M');
                self.packet_queue.push_back(b'3');
            }
            SerialMouseProtocol::MouseSystems => {
                // MouseSystems sends 'H' on reset
                self.packet_queue.push_back(b'H');
            }
        }
    }

    /// Check if there's accumulated movement or button changes to report
    fn has_changes(&self) -> bool {
        self.accum_x != 0
            || self.accum_y != 0
            || self.left_button != self.last_left
            || self.right_button != self.last_right
            || self.middle_button != self.last_middle
    }

    /// Generate a mouse packet based on current state
    pub fn generate_packet(&mut self) {
        if !self.dtr_active || !self.identified {
            return;
        }

        if !self.has_changes() {
            return;
        }

        // Clamp accumulated movement to protocol limits
        let (dx, dy) = match self.protocol {
            SerialMouseProtocol::Microsoft | SerialMouseProtocol::MicrosoftLogitech => {
                // Microsoft protocol: -128 to +127
                let dx = self.accum_x.clamp(-128, 127) as i8;
                let dy = self.accum_y.clamp(-128, 127) as i8;
                (dx as i32, dy as i32)
            }
            SerialMouseProtocol::MouseSystems => {
                // MouseSystems: -128 to +127
                let dx = self.accum_x.clamp(-128, 127) as i8;
                let dy = self.accum_y.clamp(-128, 127) as i8;
                (dx as i32, dy as i32)
            }
        };

        // Update accumulated values
        self.accum_x -= dx;
        self.accum_y -= dy;

        // Update last button state
        self.last_left = self.left_button;
        self.last_right = self.right_button;
        self.last_middle = self.middle_button;

        match self.protocol {
            SerialMouseProtocol::Microsoft => {
                self.generate_microsoft_packet(dx as i8, dy as i8, false);
            }
            SerialMouseProtocol::MicrosoftLogitech => {
                self.generate_microsoft_packet(dx as i8, dy as i8, true);
            }
            SerialMouseProtocol::MouseSystems => {
                self.generate_mousesystems_packet(dx as i8, dy as i8);
            }
        }
    }

    /// Generate Microsoft Mouse protocol packet
    ///
    /// 3-byte packet format:
    /// Byte 0: 01LRYYXX (sync bit, buttons, high bits of movement)
    /// Byte 1: 00XXXXXX (low 6 bits of X movement)
    /// Byte 2: 00YYYYYY (low 6 bits of Y movement)
    ///
    /// Logitech extension adds optional 4th byte:
    /// Byte 3: 00M00000 (middle button in bit 5)
    fn generate_microsoft_packet(&mut self, dx: i8, dy: i8, logitech: bool) {
        // Half-packets corrupt the wire stream — drop the whole report if the
        // queue can't hold all 3 (or 4) bytes atomically.
        let needed = if logitech { 4 } else { 3 };
        if self.packet_queue.len() + needed > self.packet_queue.capacity() {
            return;
        }

        let lb = if self.left_button { 0x20 } else { 0 };
        let rb = if self.right_button { 0x10 } else { 0 };

        // Extract movement bits
        let x_high = (dx as u8 >> 6) & 0x03;
        let y_high = (dy as u8 >> 6) & 0x03;
        let x_low = (dx as u8) & 0x3F;
        let y_low = (dy as u8) & 0x3F;

        // Byte 0: 01LRYY XX (sync=0x40, left, right, Y7:6, X7:6)
        let byte0 = 0x40 | lb | rb | (y_high << 2) | x_high;
        // Byte 1: 00XXXXXX (X5:0)
        let byte1 = x_low;
        // Byte 2: 00YYYYYY (Y5:0)
        let byte2 = y_low;

        self.packet_queue.push_back(byte0);
        self.packet_queue.push_back(byte1);
        self.packet_queue.push_back(byte2);

        // Logitech extension: always send 4th byte so button release is notified
        if logitech {
            // Byte 3: 00M00000 (middle button in bit 5)
            let byte3 = if self.middle_button { 0x20 } else { 0x00 };
            self.packet_queue.push_back(byte3);
        }
    }

    /// Generate MouseSystems protocol packet
    ///
    /// 5-byte packet format:
    /// Byte 0: 10000LMR (sync bits, button states inverted - 0=pressed)
    /// Byte 1: X movement (signed byte)
    /// Byte 2: Y movement (signed byte)
    /// Byte 3: X movement continuation
    /// Byte 4: Y movement continuation
    fn generate_mousesystems_packet(&mut self, dx: i8, dy: i8) {
        // 5-byte packet must land atomically on the wire. Drop the whole
        // report if the queue can't hold it.
        if self.packet_queue.len() + 5 > self.packet_queue.capacity() {
            return;
        }

        // Buttons are inverted in MouseSystems (0 = pressed)
        let lb = if self.left_button { 0 } else { 0x04 };
        let mb = if self.middle_button { 0 } else { 0x02 };
        let rb = if self.right_button { 0 } else { 0x01 };

        // MouseSystems Y is inverted compared to Microsoft
        let dy_inv = -dy;

        // Byte 0: 10000LMR (sync=0x80, buttons)
        let byte0 = 0x80 | lb | mb | rb;
        // Bytes 1-2: Movement
        let byte1 = dx as u8;
        let byte2 = dy_inv as u8;
        // Bytes 3-4: Movement continuation (typically 0 for single report)
        let byte3 = 0u8;
        let byte4 = 0u8;

        self.packet_queue.push_back(byte0);
        self.packet_queue.push_back(byte1);
        self.packet_queue.push_back(byte2);
        self.packet_queue.push_back(byte3);
        self.packet_queue.push_back(byte4);
    }

    /// Get next byte from the output queue (to send to serial port)
    pub fn get_byte(&mut self) -> Option<u8> {
        self.packet_queue.pop_front()
    }

    /// Check if there are bytes waiting to be sent
    pub fn has_pending_data(&self) -> bool {
        !self.packet_queue.is_empty()
    }

    /// Get number of bytes in queue
    pub fn pending_bytes(&self) -> usize {
        self.packet_queue.len()
    }

    /// Update timing and generate data (call periodically)
    /// Returns bytes that should be sent to the serial port receive buffer
    ///
    /// At 1200 baud with 7N1 (10 bits per character including start/stop),
    /// one byte takes approximately 8.33ms
    pub fn tick(&mut self, elapsed_ms: f64) -> Vec<u8> {
        let mut result = Vec::new();

        if !self.dtr_active {
            return result;
        }

        // Generate packets if there's movement/button changes
        self.generate_packet();

        // Time per byte at 1200 baud, 7N1 (10 bits: 1 start + 7 data + 1 parity/none + 1 stop)
        const MS_PER_BYTE: f64 = 1000.0 / (1200.0 / 10.0); // ~8.33ms

        self.byte_timer += elapsed_ms;

        while self.byte_timer >= MS_PER_BYTE {
            if let Some(byte) = self.get_byte() {
                result.push(byte);
                self.byte_timer -= MS_PER_BYTE;
            } else {
                // No more data, reset timer
                self.byte_timer = 0.0;
                break;
            }
        }

        result
    }

    /// Reset the mouse state
    pub fn reset(&mut self) {
        self.delta_x = 0;
        self.delta_y = 0;
        self.left_button = false;
        self.right_button = false;
        self.middle_button = false;
        self.packet_queue.clear();
        self.rts_active = false;
        self.dtr_active = false;
        self.last_left = false;
        self.last_right = false;
        self.last_middle = false;
        self.accum_x = 0;
        self.accum_y = 0;
        self.identified = false;
        self.byte_timer = 0.0;
    }

    /// Check if mouse is powered (DTR active)
    pub fn is_powered(&self) -> bool {
        self.dtr_active
    }

    /// Check if mouse has been identified
    pub fn is_identified(&self) -> bool {
        self.identified
    }
}

/// Serial mouse controller that manages a mouse attached to a serial port
#[derive(Debug, Clone)]
pub struct SerialMouseController {
    /// The serial mouse
    mouse: SerialMouse,
    /// COM port number this mouse is attached to (1-4)
    port_num: u8,
    /// Whether the mouse is attached/enabled
    attached: bool,
}

impl SerialMouseController {
    /// Create a new serial mouse controller
    pub fn new(port_num: u8, protocol: SerialMouseProtocol) -> Self {
        Self {
            mouse: SerialMouse::new(protocol),
            port_num,
            attached: true,
        }
    }

    /// Get the COM port number
    pub fn port_num(&self) -> u8 {
        self.port_num
    }

    /// Attach/detach the mouse
    pub fn set_attached(&mut self, attached: bool) {
        self.attached = attached;
        if !attached {
            self.mouse.reset();
        }
    }

    /// Check if mouse is attached
    pub fn is_attached(&self) -> bool {
        self.attached
    }

    /// Get mutable reference to the mouse
    pub fn mouse_mut(&mut self) -> &mut SerialMouse {
        &mut self.mouse
    }

    /// Get reference to the mouse
    pub fn mouse(&self) -> &SerialMouse {
        &self.mouse
    }

    /// Update the mouse with movement delta
    pub fn update_movement(&mut self, dx: i32, dy: i32) {
        if self.attached {
            self.mouse.update_movement(dx, dy);
        }
    }

    /// Set button states
    pub fn set_buttons(&mut self, left: bool, right: bool, middle: bool) {
        if self.attached {
            self.mouse.set_buttons(left, right, middle);
        }
    }

    /// Handle MCR (Modem Control Register) changes from serial port
    /// Extracts DTR and RTS signals
    pub fn handle_mcr(&mut self, mcr: u8) {
        if self.attached {
            // DTR is bit 0, RTS is bit 1
            self.mouse.set_dtr((mcr & 0x01) != 0);
            self.mouse.set_rts((mcr & 0x02) != 0);
        }
    }

    /// Tick the mouse and return bytes to send to serial port
    pub fn tick(&mut self, elapsed_ms: f64) -> Vec<u8> {
        if self.attached {
            self.mouse.tick(elapsed_ms)
        } else {
            Vec::new()
        }
    }

    /// Reset the controller
    pub fn reset(&mut self) {
        self.mouse.reset();
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::*;

    #[test]
    fn test_serial_mouse_creation() {
        let mouse = SerialMouse::new(SerialMouseProtocol::Microsoft);
        assert_eq!(mouse.protocol(), SerialMouseProtocol::Microsoft);
        assert!(!mouse.is_powered());
        assert!(!mouse.is_identified());
    }

    #[test]
    fn test_microsoft_identification() {
        let mut mouse = SerialMouse::new(SerialMouseProtocol::Microsoft);

        // Power on (DTR)
        mouse.set_dtr(true);
        assert!(mouse.is_powered());

        // Send reset (RTS transition)
        mouse.set_rts(true);
        assert!(mouse.is_identified());

        // Should have 'M' in queue
        assert_eq!(mouse.get_byte(), Some(b'M'));
        assert_eq!(mouse.get_byte(), None);
    }

    #[test]
    fn test_logitech_identification() {
        let mut mouse = SerialMouse::new(SerialMouseProtocol::MicrosoftLogitech);

        mouse.set_dtr(true);
        mouse.set_rts(true);

        // Should have 'M3' in queue
        assert_eq!(mouse.get_byte(), Some(b'M'));
        assert_eq!(mouse.get_byte(), Some(b'3'));
        assert_eq!(mouse.get_byte(), None);
    }

    #[test]
    fn test_mousesystems_identification() {
        let mut mouse = SerialMouse::new(SerialMouseProtocol::MouseSystems);

        mouse.set_dtr(true);
        mouse.set_rts(true);

        // Should have 'H' in queue
        assert_eq!(mouse.get_byte(), Some(b'H'));
        assert_eq!(mouse.get_byte(), None);
    }

    #[test]
    fn test_generate_packet_requires_power_and_id() {
        let mut mouse = SerialMouse::new(SerialMouseProtocol::Microsoft);

        mouse.update_movement(5, 5);
        mouse.generate_packet();

        assert!(!mouse.has_pending_data());

        mouse.set_dtr(true);
        mouse.update_movement(3, 0);
        mouse.generate_packet();
        assert!(!mouse.has_pending_data());
    }

    #[test]
    fn test_has_changes_right_middle_buttons() {
        let mut mouse = SerialMouse::new(SerialMouseProtocol::Microsoft);
        mouse.set_dtr(true);
        mouse.set_rts(true);
        mouse.get_byte(); // Clear ID

        mouse.update_movement(0, 1);
        assert!(mouse.has_changes());
        mouse.generate_packet();
        let _ = mouse.get_byte();
        let _ = mouse.get_byte();
        let _ = mouse.get_byte();

        mouse.set_buttons(false, true, false);
        assert!(mouse.has_changes());

        mouse.generate_packet();
        let _ = mouse.get_byte();
        let _ = mouse.get_byte();
        let _ = mouse.get_byte();

        mouse.set_buttons(false, true, true);
        assert!(mouse.has_changes());
    }

    #[test]
    fn test_microsoft_packet_format() {
        let mut mouse = SerialMouse::new(SerialMouseProtocol::Microsoft);
        mouse.set_dtr(true);
        mouse.set_rts(true);
        mouse.get_byte(); // Clear ID

        // Set up movement and buttons
        mouse.update_movement(10, -5);
        mouse.set_buttons(true, false, false); // Left button pressed

        mouse.generate_packet();

        // Get packet bytes
        let byte0 = mouse.get_byte().unwrap();
        let byte1 = mouse.get_byte().unwrap();
        let byte2 = mouse.get_byte().unwrap();
        assert_eq!(mouse.get_byte(), None); // No 4th byte for standard Microsoft

        // Byte 0: 01LRYY XX (sync=0x40, L=0x20, R=0x00)
        assert_eq!(byte0 & 0x40, 0x40, "Sync bit should be set");
        assert_eq!(byte0 & 0x20, 0x20, "Left button should be set");
        assert_eq!(byte0 & 0x10, 0x00, "Right button should be clear");

        // Bytes 1-2 contain movement (low 6 bits)
        let x_low = byte1 & 0x3F;
        let y_low = byte2 & 0x3F;
        assert_eq!(x_low, 10, "X movement low bits");

        // Y = -5 as 6-bit value
        let y_expected = ((-5i8) as u8) & 0x3F;
        assert_eq!(y_low, y_expected, "Y movement low bits");
    }

    #[test]
    fn test_microsoft_right_button_packet() {
        let mut mouse = SerialMouse::new(SerialMouseProtocol::Microsoft);
        mouse.set_dtr(true);
        mouse.set_rts(true);
        mouse.get_byte(); // Clear ID

        mouse.set_buttons(false, true, false);
        mouse.generate_packet();

        let byte0 = mouse.get_byte().unwrap();
        assert_eq!(byte0 & 0x10, 0x10, "Right button should be set");
    }

    #[test]
    fn test_logitech_without_middle_button() {
        let mut mouse = SerialMouse::new(SerialMouseProtocol::MicrosoftLogitech);
        mouse.set_dtr(true);
        mouse.set_rts(true);
        mouse.get_byte(); // 'M'
        mouse.get_byte(); // '3'

        mouse.update_movement(1, 1);
        mouse.set_buttons(false, false, false);
        mouse.generate_packet();

        let _byte0 = mouse.get_byte().unwrap();
        let _byte1 = mouse.get_byte().unwrap();
        let _byte2 = mouse.get_byte().unwrap();
        // Logitech always sends 4th byte (0x00 when middle not pressed)
        let byte3 = mouse.get_byte().unwrap();
        assert_eq!(
            byte3, 0x00,
            "4th byte should be 0x00 when middle not pressed"
        );
        assert_eq!(mouse.get_byte(), None);
    }

    #[test]
    fn test_logitech_middle_button() {
        let mut mouse = SerialMouse::new(SerialMouseProtocol::MicrosoftLogitech);
        mouse.set_dtr(true);
        mouse.set_rts(true);
        mouse.get_byte(); // 'M'
        mouse.get_byte(); // '3'

        // Movement with middle button
        mouse.update_movement(1, 1);
        mouse.set_buttons(false, false, true); // Middle button only

        mouse.generate_packet();

        // Should have 4 bytes
        let _byte0 = mouse.get_byte().unwrap();
        let _byte1 = mouse.get_byte().unwrap();
        let _byte2 = mouse.get_byte().unwrap();
        let byte3 = mouse.get_byte().unwrap();

        // Byte 3: middle button in bit 5
        assert_eq!(byte3 & 0x20, 0x20, "Middle button should be set");
    }

    #[test]
    fn test_mousesystems_middle_button_packet() {
        let mut mouse = SerialMouse::new(SerialMouseProtocol::MouseSystems);
        mouse.set_dtr(true);
        mouse.set_rts(true);
        mouse.get_byte(); // Clear ID

        mouse.set_buttons(false, false, true);
        mouse.generate_packet();

        let byte0 = mouse.get_byte().unwrap();
        assert_eq!(byte0 & 0x02, 0x00, "Middle button pressed (inverted)");
    }

    #[test]
    fn test_mousesystems_packet_format() {
        let mut mouse = SerialMouse::new(SerialMouseProtocol::MouseSystems);
        mouse.set_dtr(true);
        mouse.set_rts(true);
        mouse.get_byte(); // Clear ID

        mouse.update_movement(20, 15);
        mouse.set_buttons(true, true, false); // Left and right pressed

        mouse.generate_packet();

        let byte0 = mouse.get_byte().unwrap();
        let byte1 = mouse.get_byte().unwrap();
        let byte2 = mouse.get_byte().unwrap();
        let _byte3 = mouse.get_byte().unwrap();
        let _byte4 = mouse.get_byte().unwrap();

        // Byte 0: 10000LMR (buttons inverted: 0=pressed)
        assert_eq!(byte0 & 0x80, 0x80, "Sync bit should be set");
        assert_eq!(byte0 & 0x04, 0x00, "Left button pressed (inverted)");
        assert_eq!(byte0 & 0x02, 0x02, "Middle button not pressed (inverted)");
        assert_eq!(byte0 & 0x01, 0x00, "Right button pressed (inverted)");

        // Movement
        assert_eq!(byte1 as i8, 20, "X movement");
        assert_eq!(byte2 as i8, -15, "Y movement (inverted)");
    }

    #[test]
    fn test_movement_accumulation() {
        let mut mouse = SerialMouse::new(SerialMouseProtocol::Microsoft);
        mouse.set_dtr(true);
        mouse.set_rts(true);
        mouse.get_byte(); // Clear ID

        // Accumulate multiple movements
        mouse.update_movement(10, 5);
        mouse.update_movement(20, 10);
        mouse.update_movement(-5, -3);

        // Should have accumulated 25, 12
        mouse.generate_packet();

        // Consume packet
        let _byte0 = mouse.get_byte().unwrap();
        let byte1 = mouse.get_byte().unwrap();
        let byte2 = mouse.get_byte().unwrap();

        let x = byte1 & 0x3F;
        let y = byte2 & 0x3F;
        assert_eq!(x, 25, "Accumulated X");
        assert_eq!(y, 12, "Accumulated Y");
    }

    #[test]
    fn test_tick_timing() {
        let mut mouse = SerialMouse::new(SerialMouseProtocol::Microsoft);
        mouse.set_dtr(true);
        mouse.set_rts(true);

        // At 1200 baud, 7N1: 10 bits per byte = 8.33ms per byte
        // Small tick shouldn't produce output yet
        let bytes = mouse.tick(1.0);
        assert!(bytes.is_empty(), "Should not produce output after 1ms");

        // After 5ms total (still under 8.33ms threshold)
        let bytes = mouse.tick(4.0);
        assert!(
            bytes.is_empty(),
            "Should not produce output after 5ms total"
        );

        // After 10ms total (> 8.33ms), should get first byte
        let bytes = mouse.tick(5.0);
        assert_eq!(bytes.len(), 1, "Should produce one byte after ~10ms total");
        assert_eq!(bytes[0], b'M', "Should be identification byte");
    }

    #[test]
    fn test_tick_without_pending_data_resets_timer() {
        let mut mouse = SerialMouse::new(SerialMouseProtocol::Microsoft);
        mouse.set_dtr(true);

        let _ = mouse.tick(10.0);
        assert_eq!(mouse.byte_timer, 0.0);
    }

    #[test]
    fn test_tick_requires_dtr() {
        let mut mouse = SerialMouse::new(SerialMouseProtocol::Microsoft);
        let bytes = mouse.tick(10.0);
        assert!(bytes.is_empty());
    }

    #[test]
    fn test_controller() {
        let mut ctrl = SerialMouseController::new(1, SerialMouseProtocol::Microsoft);
        assert_eq!(ctrl.port_num(), 1);
        assert!(ctrl.is_attached());

        // Detach
        ctrl.set_attached(false);
        assert!(!ctrl.is_attached());

        // Movement should be ignored when detached
        ctrl.update_movement(100, 100);
        let bytes = ctrl.tick(100.0);
        assert!(bytes.is_empty());
    }

    #[test]
    fn test_controller_set_attached_true_does_not_reset() {
        let mut ctrl = SerialMouseController::new(1, SerialMouseProtocol::Microsoft);
        ctrl.handle_mcr(0x03);
        assert!(ctrl.mouse().is_identified());

        ctrl.set_attached(true);
        assert!(ctrl.mouse().is_identified());
    }

    #[test]
    fn test_controller_ignores_button_set_when_detached() {
        let mut ctrl = SerialMouseController::new(1, SerialMouseProtocol::Microsoft);
        ctrl.set_attached(false);

        ctrl.set_buttons(true, true, true);
        assert!(!ctrl.mouse().left_button);
        assert!(!ctrl.mouse().right_button);
        assert!(!ctrl.mouse().middle_button);
    }

    #[test]
    fn test_controller_ignores_mcr_when_detached() {
        let mut ctrl = SerialMouseController::new(1, SerialMouseProtocol::Microsoft);
        ctrl.set_attached(false);

        ctrl.handle_mcr(0x03);
        assert!(!ctrl.mouse().is_powered());
    }

    #[test]
    fn test_controller_attached_paths() {
        let mut ctrl = SerialMouseController::new(1, SerialMouseProtocol::Microsoft);

        ctrl.handle_mcr(0x03);
        let _ = ctrl.tick(10.0);

        ctrl.update_movement(5, -5);
        ctrl.set_buttons(true, false, false);

        let bytes = ctrl.tick(10.0);
        assert!(!bytes.is_empty());
    }

    #[test]
    fn test_controller_mcr_handling() {
        let mut ctrl = SerialMouseController::new(2, SerialMouseProtocol::Microsoft);

        // Simulate MCR with DTR and RTS set
        ctrl.handle_mcr(0x03); // DTR=1, RTS=1

        assert!(ctrl.mouse().is_powered());
        assert!(ctrl.mouse().is_identified());
    }

    #[test]
    fn test_rts_requires_dtr_for_identification() {
        let mut mouse = SerialMouse::new(SerialMouseProtocol::Microsoft);

        mouse.set_rts(true);
        assert!(!mouse.is_identified());

        mouse.set_dtr(true);
        mouse.set_rts(false);
        mouse.set_rts(true);
        assert!(mouse.is_identified());
    }

    #[test]
    fn test_dtr_reassert_no_reset() {
        let mut mouse = SerialMouse::new(SerialMouseProtocol::Microsoft);
        mouse.set_dtr(true);
        mouse.set_rts(true);
        assert!(mouse.is_identified());

        mouse.set_dtr(true);
        assert!(mouse.is_identified());
    }

    #[test]
    fn test_rts_reassert_no_extra_identification() {
        let mut mouse = SerialMouse::new(SerialMouseProtocol::Microsoft);
        mouse.set_dtr(true);
        mouse.set_rts(true);
        mouse.get_byte();

        mouse.set_rts(true);
        assert_eq!(mouse.get_byte(), None);
    }

    #[test]
    fn test_rts_double_deassert_no_identification() {
        let mut mouse = SerialMouse::new(SerialMouseProtocol::Microsoft);

        mouse.set_rts(false);
        assert!(!mouse.is_identified());
    }

    #[test]
    fn test_dtr_deassert_does_not_reset() {
        let mut mouse = SerialMouse::new(SerialMouseProtocol::Microsoft);
        mouse.set_dtr(true);
        mouse.set_rts(true);
        assert!(mouse.is_identified());

        mouse.set_dtr(false);
        assert!(mouse.is_identified());
    }

    #[test]
    fn test_dtr_double_deassert_no_reset() {
        let mut mouse = SerialMouse::new(SerialMouseProtocol::Microsoft);

        mouse.set_dtr(false);
        mouse.set_dtr(false);
        assert!(!mouse.is_powered());
    }

    #[test]
    fn test_movement_clamping() {
        let mut mouse = SerialMouse::new(SerialMouseProtocol::Microsoft);
        mouse.set_dtr(true);
        mouse.set_rts(true);
        mouse.get_byte(); // Clear ID

        // Large movement that exceeds limits
        mouse.update_movement(200, -200);
        mouse.generate_packet();

        // Get packet
        let byte0 = mouse.get_byte().unwrap();
        let byte1 = mouse.get_byte().unwrap();
        let byte2 = mouse.get_byte().unwrap();

        // Reconstruct movement
        let x_high = (byte0 & 0x03) << 6;
        let x_low = byte1 & 0x3F;
        let x = ((x_high | x_low) as i8) as i32;

        let y_high = ((byte0 >> 2) & 0x03) << 6;
        let y_low = byte2 & 0x3F;
        let y = ((y_high | y_low) as i8) as i32;

        // Should be clamped to -128..127
        assert_eq!(x, 127, "X should be clamped to 127");
        assert_eq!(y, -128, "Y should be clamped to -128");
    }

    #[test]
    fn test_no_packet_without_changes() {
        let mut mouse = SerialMouse::new(SerialMouseProtocol::Microsoft);
        mouse.set_dtr(true);
        mouse.set_rts(true);
        mouse.get_byte(); // Clear ID

        // No movement or button changes
        mouse.generate_packet();

        // Should have no packet
        assert!(!mouse.has_pending_data());
    }

    #[test]
    fn test_button_change_generates_packet() {
        let mut mouse = SerialMouse::new(SerialMouseProtocol::Microsoft);
        mouse.set_dtr(true);
        mouse.set_rts(true);
        mouse.get_byte(); // Clear ID

        // Just button change, no movement
        mouse.set_buttons(true, false, false);
        mouse.generate_packet();

        // Should have packet with zero movement
        assert!(mouse.has_pending_data());
    }

    #[test]
    fn test_reset() {
        let mut mouse = SerialMouse::new(SerialMouseProtocol::Microsoft);
        mouse.set_dtr(true);
        mouse.set_rts(true);
        mouse.update_movement(50, 50);
        mouse.set_buttons(true, true, true);

        mouse.reset();

        assert!(!mouse.is_powered());
        assert!(!mouse.is_identified());
        assert!(!mouse.has_pending_data());
    }
}
