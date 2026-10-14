//! x86 CPU flags register

use bitflags::bitflags;
use serde::{Deserialize, Serialize};

bitflags! {
    /// x86 FLAGS/EFLAGS register
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
    pub struct Flags: u16 {
        /// Carry flag
        const CARRY     = 1 << 0;
        /// Reserved (always 1)
        const RESERVED1 = 1 << 1;
        /// Parity flag
        const PARITY    = 1 << 2;
        /// Auxiliary carry flag (BCD)
        const AUX_CARRY = 1 << 4;
        /// Zero flag
        const ZERO      = 1 << 6;
        /// Sign flag
        const SIGN      = 1 << 7;
        /// Trap flag (single step)
        const TRAP      = 1 << 8;
        /// Interrupt enable flag
        const INTERRUPT = 1 << 9;
        /// Direction flag
        const DIRECTION = 1 << 10;
        /// Overflow flag
        const OVERFLOW  = 1 << 11;
        /// I/O privilege level (bit 0)
        const IOPL0     = 1 << 12;
        /// I/O privilege level (bit 1)
        const IOPL1     = 1 << 13;
        /// Nested task flag
        const NESTED    = 1 << 14;
    }
}

impl Flags {
    /// Update flags for 8-bit result
    #[inline]
    pub fn update_sz8(&mut self, result: u8) {
        // Build new flags in one operation, preserve other flags
        let mut new_flags = *self & !(Flags::ZERO | Flags::SIGN | Flags::PARITY);
        if result == 0 {
            new_flags |= Flags::ZERO;
        }
        if (result & 0x80) != 0 {
            new_flags |= Flags::SIGN;
        }
        if result.count_ones() & 1 == 0 {
            new_flags |= Flags::PARITY;
        }
        *self = new_flags;
    }

    /// Update flags for 16-bit result
    #[inline]
    pub fn update_sz16(&mut self, result: u16) {
        let mut new_flags = *self & !(Flags::ZERO | Flags::SIGN | Flags::PARITY);
        if result == 0 {
            new_flags |= Flags::ZERO;
        }
        if (result & 0x8000) != 0 {
            new_flags |= Flags::SIGN;
        }
        if (result as u8).count_ones() & 1 == 0 {
            new_flags |= Flags::PARITY;
        }
        *self = new_flags;
    }

    /// Update flags for 32-bit result
    #[inline]
    pub fn update_sz32(&mut self, result: u32) {
        let mut new_flags = *self & !(Flags::ZERO | Flags::SIGN | Flags::PARITY);
        if result == 0 {
            new_flags |= Flags::ZERO;
        }
        if (result & 0x8000_0000) != 0 {
            new_flags |= Flags::SIGN;
        }
        if (result as u8).count_ones() & 1 == 0 {
            new_flags |= Flags::PARITY;
        }
        *self = new_flags;
    }

    /// Update flags after 8-bit addition
    #[inline]
    pub fn update_add8(&mut self, a: u8, b: u8, result: u8) {
        self.update_sz8(result);
        self.set(Flags::CARRY, (a as u16 + b as u16) > 0xFF);
        self.set(Flags::OVERFLOW, ((a ^ result) & (b ^ result) & 0x80) != 0);
        self.set(Flags::AUX_CARRY, ((a & 0x0F) + (b & 0x0F)) > 0x0F);
    }

    /// Update flags after 8-bit ADC (add with carry)
    #[inline]
    pub fn update_adc8(&mut self, a: u8, b: u8, carry: u8, result: u8) {
        self.update_sz8(result);
        let sum = a as u16 + b as u16 + carry as u16;
        self.set(Flags::CARRY, sum > 0xFF);
        self.set(Flags::OVERFLOW, ((a ^ result) & (b ^ result) & 0x80) != 0);
        // AF must include carry in the low nibble calculation
        self.set(Flags::AUX_CARRY, ((a & 0x0F) + (b & 0x0F) + carry) > 0x0F);
    }

    /// Update flags after 16-bit addition
    #[inline]
    pub fn update_add16(&mut self, a: u16, b: u16, result: u16) {
        self.update_sz16(result);
        self.set(Flags::CARRY, (a as u32 + b as u32) > 0xFFFF);
        self.set(Flags::OVERFLOW, ((a ^ result) & (b ^ result) & 0x8000) != 0);
        self.set(Flags::AUX_CARRY, ((a & 0x0F) + (b & 0x0F)) > 0x0F);
    }

    /// Update flags after 16-bit ADC (add with carry)
    #[inline]
    pub fn update_adc16(&mut self, a: u16, b: u16, carry: u16, result: u16) {
        self.update_sz16(result);
        let sum = a as u32 + b as u32 + carry as u32;
        self.set(Flags::CARRY, sum > 0xFFFF);
        self.set(Flags::OVERFLOW, ((a ^ result) & (b ^ result) & 0x8000) != 0);
        // AF must include carry in the low nibble calculation
        self.set(Flags::AUX_CARRY, ((a & 0x0F) + (b & 0x0F) + carry) > 0x0F);
    }

    /// Update flags after 8-bit subtraction
    #[inline]
    pub fn update_sub8(&mut self, a: u8, b: u8, result: u8) {
        self.update_sz8(result);
        self.set(Flags::CARRY, a < b);
        self.set(Flags::OVERFLOW, ((a ^ b) & (a ^ result) & 0x80) != 0);
        self.set(Flags::AUX_CARRY, (a & 0x0F) < (b & 0x0F));
    }

    /// Update flags after 16-bit subtraction
    #[inline]
    pub fn update_sub16(&mut self, a: u16, b: u16, result: u16) {
        self.update_sz16(result);
        self.set(Flags::CARRY, a < b);
        self.set(Flags::OVERFLOW, ((a ^ b) & (a ^ result) & 0x8000) != 0);
        self.set(Flags::AUX_CARRY, (a & 0x0F) < (b & 0x0F));
    }

    /// Update flags after 32-bit addition
    pub fn update_add32(&mut self, a: u32, b: u32, result: u32) {
        self.update_sz32(result);
        self.set(Flags::CARRY, (a as u64 + b as u64) > 0xFFFF_FFFF);
        self.set(
            Flags::OVERFLOW,
            ((a ^ result) & (b ^ result) & 0x8000_0000) != 0,
        );
        self.set(Flags::AUX_CARRY, ((a & 0x0F) + (b & 0x0F)) > 0x0F);
    }

    /// Update flags after 32-bit subtraction
    pub fn update_sub32(&mut self, a: u32, b: u32, result: u32) {
        self.update_sz32(result);
        self.set(Flags::CARRY, a < b);
        self.set(Flags::OVERFLOW, ((a ^ b) & (a ^ result) & 0x8000_0000) != 0);
        self.set(Flags::AUX_CARRY, (a & 0x0F) < (b & 0x0F));
    }

    /// Update flags after 32-bit ADC (add with carry)
    #[inline]
    pub fn update_adc32(&mut self, a: u32, b: u32, carry: u32, result: u32) {
        self.update_sz32(result);
        let sum = a as u64 + b as u64 + carry as u64;
        self.set(Flags::CARRY, sum > 0xFFFF_FFFF);
        self.set(
            Flags::OVERFLOW,
            ((a ^ result) & (b ^ result) & 0x8000_0000) != 0,
        );
        // AF must include carry in the low nibble calculation
        self.set(Flags::AUX_CARRY, ((a & 0x0F) + (b & 0x0F) + carry) > 0x0F);
    }

    /// Update flags after 32-bit SBB (subtract with borrow)
    #[inline]
    pub fn update_sbb32(&mut self, a: u32, b: u32, borrow: u32, result: u32) {
        self.update_sz32(result);
        // CF set if borrow needed (a < b + borrow)
        self.set(Flags::CARRY, (a as u64) < (b as u64 + borrow as u64));
        self.set(Flags::OVERFLOW, ((a ^ b) & (a ^ result) & 0x8000_0000) != 0);
        // AF for low nibble borrow
        self.set(Flags::AUX_CARRY, (a & 0x0F) < ((b & 0x0F) + borrow));
    }

    /// Update flags after logical operation (AND, OR, XOR, TEST)
    /// Clears CF, OF, and AF (AF is undefined but real 386 clears it)
    pub fn update_logic8(&mut self, result: u8) {
        self.update_sz8(result);
        self.remove(Flags::CARRY | Flags::OVERFLOW | Flags::AUX_CARRY);
    }

    /// Update flags after logical operation (AND, OR, XOR, TEST)
    /// Clears CF, OF, and AF (AF is undefined but real 386 clears it)
    pub fn update_logic16(&mut self, result: u16) {
        self.update_sz16(result);
        self.remove(Flags::CARRY | Flags::OVERFLOW | Flags::AUX_CARRY);
    }

    /// Update flags after 32-bit logical operation (AND, OR, XOR, TEST)
    /// Clears CF, OF, and AF (AF is undefined but real 386 clears it)
    pub fn update_logic32(&mut self, result: u32) {
        self.update_sz32(result);
        self.remove(Flags::CARRY | Flags::OVERFLOW | Flags::AUX_CARRY);
    }

    pub fn get_zf(&self) -> bool {
        self.contains(Flags::ZERO)
    }

    pub fn get_cf(&self) -> bool {
        self.contains(Flags::CARRY)
    }

    pub fn get_df(&self) -> bool {
        self.contains(Flags::DIRECTION)
    }

    pub fn get_of(&self) -> bool {
        self.contains(Flags::OVERFLOW)
    }

    pub fn get_sf(&self) -> bool {
        self.contains(Flags::SIGN)
    }

    pub fn get_pf(&self) -> bool {
        self.contains(Flags::PARITY)
    }
}

impl Default for Flags {
    fn default() -> Self {
        Flags::RESERVED1 // Bit 1 is always set
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::*;

    #[test]
    fn test_zero_flag() {
        let mut flags = Flags::empty();
        flags.update_sz8(0);
        assert!(flags.contains(Flags::ZERO));

        flags.update_sz8(1);
        assert!(!flags.contains(Flags::ZERO));
    }

    #[test]
    fn test_sign_flag() {
        let mut flags = Flags::empty();
        flags.update_sz8(0x80);
        assert!(flags.contains(Flags::SIGN));

        flags.update_sz8(0x7F);
        assert!(!flags.contains(Flags::SIGN));
    }

    #[test]
    fn test_carry_add() {
        let mut flags = Flags::empty();
        flags.update_add8(0xFF, 0x01, 0x00);
        assert!(flags.contains(Flags::CARRY));

        flags.update_add8(0x01, 0x01, 0x02);
        assert!(!flags.contains(Flags::CARRY));
    }

    #[test]
    fn test_overflow_add() {
        let mut flags = Flags::empty();
        // 127 + 1 = 128, but 128 is -128 in signed, so overflow
        flags.update_add8(0x7F, 0x01, 0x80);
        assert!(flags.contains(Flags::OVERFLOW));
    }

    #[test]
    fn test_parity_flag() {
        let mut flags = Flags::empty();
        // 0x00 has 0 ones (even parity)
        flags.update_sz8(0x00);
        assert!(flags.contains(Flags::PARITY));

        // 0x01 has 1 one (odd parity)
        flags.update_sz8(0x01);
        assert!(!flags.contains(Flags::PARITY));

        // 0x03 has 2 ones (even parity)
        flags.update_sz8(0x03);
        assert!(flags.contains(Flags::PARITY));
    }

    #[test]
    fn test_update_sz16() {
        let mut flags = Flags::empty();

        // Zero
        flags.update_sz16(0);
        assert!(flags.contains(Flags::ZERO));
        assert!(!flags.contains(Flags::SIGN));

        // Positive
        flags.update_sz16(0x1234);
        assert!(!flags.contains(Flags::ZERO));
        assert!(!flags.contains(Flags::SIGN));

        // Negative (sign bit set)
        flags.update_sz16(0x8000);
        assert!(!flags.contains(Flags::ZERO));
        assert!(flags.contains(Flags::SIGN));
    }

    #[test]
    fn test_update_sz32() {
        let mut flags = Flags::empty();

        // Zero
        flags.update_sz32(0);
        assert!(flags.contains(Flags::ZERO));
        assert!(!flags.contains(Flags::SIGN));

        // Positive
        flags.update_sz32(0x12345678);
        assert!(!flags.contains(Flags::ZERO));
        assert!(!flags.contains(Flags::SIGN));

        // Negative (sign bit set)
        flags.update_sz32(0x80000000);
        assert!(!flags.contains(Flags::ZERO));
        assert!(flags.contains(Flags::SIGN));
    }

    #[test]
    fn test_update_add16() {
        let mut flags = Flags::empty();

        // No carry
        flags.update_add16(0x1000, 0x2000, 0x3000);
        assert!(!flags.contains(Flags::CARRY));
        assert!(!flags.contains(Flags::OVERFLOW));

        // Carry (16-bit overflow)
        flags.update_add16(0xFFFF, 0x0001, 0x0000);
        assert!(flags.contains(Flags::CARRY));
        assert!(flags.contains(Flags::ZERO));

        // Signed overflow: 0x7FFF + 1 = 0x8000 (-32768)
        flags.update_add16(0x7FFF, 0x0001, 0x8000);
        assert!(flags.contains(Flags::OVERFLOW));
        assert!(flags.contains(Flags::SIGN));
    }

    #[test]
    fn test_update_add32() {
        let mut flags = Flags::empty();

        // No carry
        flags.update_add32(0x10000000, 0x20000000, 0x30000000);
        assert!(!flags.contains(Flags::CARRY));

        // Carry (32-bit overflow)
        flags.update_add32(0xFFFFFFFF, 0x00000001, 0x00000000);
        assert!(flags.contains(Flags::CARRY));

        // Signed overflow
        flags.update_add32(0x7FFFFFFF, 0x00000001, 0x80000000);
        assert!(flags.contains(Flags::OVERFLOW));
    }

    #[test]
    fn test_update_sub8() {
        let mut flags = Flags::empty();

        // No borrow
        flags.update_sub8(0x10, 0x05, 0x0B);
        assert!(!flags.contains(Flags::CARRY));

        // Borrow (a < b)
        flags.update_sub8(0x05, 0x10, 0xF5);
        assert!(flags.contains(Flags::CARRY));

        // Zero result
        flags.update_sub8(0x42, 0x42, 0x00);
        assert!(flags.contains(Flags::ZERO));

        // Signed overflow: 0x80 - 1 = 0x7F (overflow from negative to positive)
        flags.update_sub8(0x80, 0x01, 0x7F);
        assert!(flags.contains(Flags::OVERFLOW));
    }

    #[test]
    fn test_update_sub16() {
        let mut flags = Flags::empty();

        // No borrow
        flags.update_sub16(0x1000, 0x0500, 0x0B00);
        assert!(!flags.contains(Flags::CARRY));

        // Borrow
        flags.update_sub16(0x0500, 0x1000, 0xF500);
        assert!(flags.contains(Flags::CARRY));

        // Signed overflow
        flags.update_sub16(0x8000, 0x0001, 0x7FFF);
        assert!(flags.contains(Flags::OVERFLOW));
    }

    #[test]
    fn test_update_sub32() {
        let mut flags = Flags::empty();

        // No borrow
        flags.update_sub32(0x10000000, 0x05000000, 0x0B000000);
        assert!(!flags.contains(Flags::CARRY));

        // Borrow
        flags.update_sub32(0x05000000, 0x10000000, 0xF5000000);
        assert!(flags.contains(Flags::CARRY));

        // Signed overflow
        flags.update_sub32(0x80000000, 0x00000001, 0x7FFFFFFF);
        assert!(flags.contains(Flags::OVERFLOW));
    }

    #[test]
    fn test_aux_carry_add8() {
        let mut flags = Flags::empty();

        // Aux carry: low nibble overflow
        flags.update_add8(0x0F, 0x01, 0x10);
        assert!(flags.contains(Flags::AUX_CARRY));

        // No aux carry
        flags.update_add8(0x01, 0x01, 0x02);
        assert!(!flags.contains(Flags::AUX_CARRY));
    }

    #[test]
    fn test_aux_carry_sub8() {
        let mut flags = Flags::empty();

        // Aux borrow: low nibble borrow
        flags.update_sub8(0x10, 0x01, 0x0F);
        assert!(flags.contains(Flags::AUX_CARRY));

        // No aux borrow
        flags.update_sub8(0x0F, 0x01, 0x0E);
        assert!(!flags.contains(Flags::AUX_CARRY));
    }

    #[test]
    fn test_update_adc8() {
        let mut flags = Flags::empty();

        // 1. Simple add with carry in
        // 1 + 2 + 1(c) = 4
        flags.update_adc8(1, 2, 1, 4);
        assert!(!flags.contains(Flags::CARRY));
        assert!(!flags.contains(Flags::OVERFLOW));
        assert!(!flags.contains(Flags::AUX_CARRY));

        // 2. Carry generated by sum + carry
        // 0xFF + 0 + 1 = 0x00 (Carry)
        flags.update_adc8(0xFF, 0, 1, 0x00);
        assert!(flags.contains(Flags::CARRY));
        assert!(flags.contains(Flags::ZERO));
        assert!(flags.contains(Flags::AUX_CARRY)); // 0xF + 0 + 1 = 0x10 (AF)

        // 3. Overflow with carry
        // 0x7F (127) + 0 + 1 = 0x80 (-128)
        flags.update_adc8(0x7F, 0, 1, 0x80);
        assert!(flags.contains(Flags::OVERFLOW));
        assert!(flags.contains(Flags::SIGN));
        assert!(flags.contains(Flags::AUX_CARRY)); // 0xF + 0 + 1 = 0x10 (AF)

        // 4. AF generated by carry only
        // 0x0F + 0 + 1 = 0x10
        flags.update_adc8(0x0F, 0, 1, 0x10);
        assert!(flags.contains(Flags::AUX_CARRY));
    }

    #[test]
    fn test_update_adc16() {
        let mut flags = Flags::empty();

        // 1. Carry generated by sum + carry
        // 0xFFFF + 0 + 1 = 0x0000 (Carry)
        flags.update_adc16(0xFFFF, 0, 1, 0x0000);
        assert!(flags.contains(Flags::CARRY));
        assert!(flags.contains(Flags::ZERO));

        // 2. Overflow with carry
        // 0x7FFF + 0 + 1 = 0x8000
        flags.update_adc16(0x7FFF, 0, 1, 0x8000);
        assert!(flags.contains(Flags::OVERFLOW));
        assert!(flags.contains(Flags::SIGN));
    }

    #[test]
    fn test_update_adc32() {
        let mut flags = Flags::empty();

        // 1. Carry generated by sum + carry
        // 0xFFFFFFFF + 0 + 1 = 0x00000000
        flags.update_adc32(0xFFFFFFFF, 0, 1, 0x00000000);
        assert!(flags.contains(Flags::CARRY));
        assert!(flags.contains(Flags::ZERO));

        // 2. Overflow with carry
        // 0x7FFFFFFF + 0 + 1 = 0x80000000
        flags.update_adc32(0x7FFFFFFF, 0, 1, 0x80000000);
        assert!(flags.contains(Flags::OVERFLOW));
    }

    #[test]
    fn test_update_sbb32() {
        let mut flags = Flags::empty();

        // 1. Simple sub with borrow
        // 5 - 1 - 1 = 3
        flags.update_sbb32(5, 1, 1, 3);
        assert!(!flags.contains(Flags::CARRY)); // No borrow generated

        // 2. Borrow generated by borrow input
        // 0 - 0 - 1 = 0xFFFFFFFF
        flags.update_sbb32(0, 0, 1, 0xFFFFFFFF);
        assert!(flags.contains(Flags::CARRY)); // Borrow occurred
        assert!(flags.contains(Flags::SIGN));

        // 3. Overflow with borrow
        // 0x80000000 (-MAX) - 0 - 1 = 0x7FFFFFFF (MAX)
        flags.update_sbb32(0x80000000, 0, 1, 0x7FFFFFFF);
        assert!(flags.contains(Flags::OVERFLOW));
    }

    #[test]
    fn test_update_logic8() {
        let mut flags = Flags::CARRY | Flags::OVERFLOW | Flags::AUX_CARRY;

        flags.update_logic8(0x00);
        // Logic ops clear carry, overflow, and aux carry
        assert!(!flags.contains(Flags::CARRY));
        assert!(!flags.contains(Flags::OVERFLOW));
        assert!(!flags.contains(Flags::AUX_CARRY));
        // But set zero
        assert!(flags.contains(Flags::ZERO));

        flags.update_logic8(0x80);
        assert!(flags.contains(Flags::SIGN));
    }

    #[test]
    fn test_update_logic16() {
        let mut flags = Flags::CARRY | Flags::OVERFLOW | Flags::AUX_CARRY;

        flags.update_logic16(0x0000);
        assert!(!flags.contains(Flags::CARRY));
        assert!(!flags.contains(Flags::OVERFLOW));
        assert!(!flags.contains(Flags::AUX_CARRY));
        assert!(flags.contains(Flags::ZERO));

        flags.update_logic16(0x8000);
        assert!(flags.contains(Flags::SIGN));
    }

    #[test]
    fn test_update_logic32() {
        let mut flags = Flags::CARRY | Flags::OVERFLOW | Flags::AUX_CARRY;

        // Zero result
        flags.update_logic32(0);
        assert!(!flags.contains(Flags::CARRY));
        assert!(!flags.contains(Flags::OVERFLOW));
        assert!(!flags.contains(Flags::AUX_CARRY));
        assert!(flags.contains(Flags::ZERO));
        assert!(flags.contains(Flags::PARITY)); // 0 has even parity

        // Sign result
        flags.update_logic32(0x80000000);
        assert!(flags.contains(Flags::SIGN));
    }

    #[test]
    fn test_default() {
        let flags = Flags::default();
        // Bit 1 (RESERVED1) should be set
        assert!(flags.contains(Flags::RESERVED1));
    }

    #[test]
    fn test_flag_bits() {
        // Verify flag bit positions
        assert_eq!(Flags::CARRY.bits(), 0x0001);
        assert_eq!(Flags::PARITY.bits(), 0x0004);
        assert_eq!(Flags::AUX_CARRY.bits(), 0x0010);
        assert_eq!(Flags::ZERO.bits(), 0x0040);
        assert_eq!(Flags::SIGN.bits(), 0x0080);
        assert_eq!(Flags::TRAP.bits(), 0x0100);
        assert_eq!(Flags::INTERRUPT.bits(), 0x0200);
        assert_eq!(Flags::DIRECTION.bits(), 0x0400);
        assert_eq!(Flags::OVERFLOW.bits(), 0x0800);
    }

    #[test]
    fn test_flags_preserve_other_bits() {
        let mut flags = Flags::INTERRUPT | Flags::DIRECTION | Flags::CARRY;

        flags.update_sz8(0x42);

        // INTERRUPT and DIRECTION should be preserved
        assert!(flags.contains(Flags::INTERRUPT));
        assert!(flags.contains(Flags::DIRECTION));
        // CARRY is not affected by update_sz8
        assert!(flags.contains(Flags::CARRY));
    }

    #[test]
    fn test_parity_16bit_uses_low_byte() {
        let mut flags = Flags::empty();

        // 0x0100 - low byte is 0x00 (0 ones, even parity)
        flags.update_sz16(0x0100);
        assert!(flags.contains(Flags::PARITY));

        // 0x0101 - low byte is 0x01 (1 one, odd parity)
        flags.update_sz16(0x0101);
        assert!(!flags.contains(Flags::PARITY));
    }

    #[test]
    fn test_parity_32bit_uses_low_byte() {
        let mut flags = Flags::empty();

        // 0x01000000 - low byte is 0x00 (even parity)
        flags.update_sz32(0x01000000);
        assert!(flags.contains(Flags::PARITY));

        // 0x00000001 - low byte is 0x01 (odd parity)
        flags.update_sz32(0x00000001);
        assert!(!flags.contains(Flags::PARITY));
    }

    #[test]
    fn test_get_zf() {
        let mut flags = Flags::empty();
        assert!(!flags.get_zf());
        flags.insert(Flags::ZERO);
        assert!(flags.get_zf());
    }

    #[test]
    fn test_get_cf() {
        let mut flags = Flags::empty();
        assert!(!flags.get_cf());
        flags.insert(Flags::CARRY);
        assert!(flags.get_cf());
    }

    #[test]
    fn test_get_df() {
        let mut flags = Flags::empty();
        assert!(!flags.get_df());
        flags.insert(Flags::DIRECTION);
        assert!(flags.get_df());
    }

    #[test]
    fn test_get_of() {
        let mut flags = Flags::empty();
        assert!(!flags.get_of());
        flags.insert(Flags::OVERFLOW);
        assert!(flags.get_of());
    }

    #[test]
    fn test_get_sf() {
        let mut flags = Flags::empty();
        assert!(!flags.get_sf());
        flags.insert(Flags::SIGN);
        assert!(flags.get_sf());
    }

    #[test]
    fn test_get_pf() {
        let mut flags = Flags::empty();
        assert!(!flags.get_pf());
        flags.insert(Flags::PARITY);
        assert!(flags.get_pf());
    }

    #[test]
    fn test_flags_debug() {
        let flags = Flags::ZERO | Flags::CARRY;
        let debug = format!("{flags:?}");
        assert!(debug.contains("ZERO"));
        assert!(debug.contains("CARRY"));
    }

    #[test]
    #[allow(clippy::clone_on_copy)]
    fn test_flags_clone_copy() {
        let flags = Flags::ZERO | Flags::CARRY;
        let copied = flags;
        let cloned = flags.clone();
        assert_eq!(flags, copied);
        assert_eq!(flags, cloned);
    }

    #[test]
    fn test_flags_eq() {
        let flags1 = Flags::ZERO | Flags::CARRY;
        let flags2 = Flags::ZERO | Flags::CARRY;
        let flags3 = Flags::ZERO;
        assert_eq!(flags1, flags2);
        assert_ne!(flags1, flags3);
    }

    // ---- SBB8/SBB16 missing coverage ----

    #[test]
    fn test_update_sbb32_no_borrow() {
        let mut flags = Flags::empty();
        // 10 - 3 - 0 = 7
        flags.update_sbb32(10, 3, 0, 7);
        assert!(!flags.contains(Flags::CARRY));
        assert!(!flags.contains(Flags::OVERFLOW));
        assert!(!flags.contains(Flags::ZERO));
    }

    #[test]
    fn test_update_sbb32_aux_carry() {
        let mut flags = Flags::empty();
        // 0x10 - 0x01 - 1 = 0x0E => AF set (0x0 < 0x1 + 1)
        flags.update_sbb32(0x10, 0x01, 1, 0x0E);
        assert!(flags.contains(Flags::AUX_CARRY));
    }

    #[test]
    fn test_update_sbb32_zero_result() {
        let mut flags = Flags::empty();
        // 5 - 4 - 1 = 0
        flags.update_sbb32(5, 4, 1, 0);
        assert!(flags.contains(Flags::ZERO));
    }

    // ---- ADC carry boundary tests ----

    #[test]
    fn test_update_adc8_no_carry_no_overflow() {
        let mut flags = Flags::empty();
        flags.update_adc8(0x10, 0x20, 0, 0x30);
        assert!(!flags.contains(Flags::CARRY));
        assert!(!flags.contains(Flags::OVERFLOW));
    }

    #[test]
    fn test_update_adc16_no_carry() {
        let mut flags = Flags::empty();
        flags.update_adc16(0x1000, 0x2000, 0, 0x3000);
        assert!(!flags.contains(Flags::CARRY));
        assert!(!flags.contains(Flags::OVERFLOW));
    }

    #[test]
    fn test_update_adc32_no_carry() {
        let mut flags = Flags::empty();
        flags.update_adc32(0x10000000, 0x20000000, 0, 0x30000000);
        assert!(!flags.contains(Flags::CARRY));
        assert!(!flags.contains(Flags::OVERFLOW));
    }

    // ---- Aux carry edge cases ----

    #[test]
    fn test_update_add16_aux_carry() {
        let mut flags = Flags::empty();
        flags.update_add16(0x000F, 0x0001, 0x0010);
        assert!(flags.contains(Flags::AUX_CARRY));
    }

    #[test]
    fn test_update_add32_aux_carry() {
        let mut flags = Flags::empty();
        flags.update_add32(0x0000000F, 0x00000001, 0x00000010);
        assert!(flags.contains(Flags::AUX_CARRY));
    }

    #[test]
    fn test_update_sub16_aux_carry() {
        let mut flags = Flags::empty();
        flags.update_sub16(0x0010, 0x0001, 0x000F);
        assert!(flags.contains(Flags::AUX_CARRY));
    }

    #[test]
    fn test_update_sub32_aux_carry() {
        let mut flags = Flags::empty();
        flags.update_sub32(0x00000010, 0x00000001, 0x0000000F);
        assert!(flags.contains(Flags::AUX_CARRY));
    }

    #[test]
    fn test_update_adc16_aux_carry() {
        let mut flags = Flags::empty();
        // 0xF + 0x0 + 1 = 0x10 => AF
        flags.update_adc16(0x000F, 0x0000, 1, 0x0010);
        assert!(flags.contains(Flags::AUX_CARRY));
    }

    #[test]
    fn test_update_adc32_aux_carry() {
        let mut flags = Flags::empty();
        flags.update_adc32(0x0000000F, 0x00000000, 1, 0x00000010);
        assert!(flags.contains(Flags::AUX_CARRY));
    }

    // ---- Parity on non-zero results ----

    #[test]
    fn test_parity_flag_8bit_even() {
        let mut flags = Flags::empty();
        // 0xFF has 8 ones (even parity)
        flags.update_sz8(0xFF);
        assert!(flags.contains(Flags::PARITY));
    }

    #[test]
    fn test_parity_flag_8bit_odd() {
        let mut flags = Flags::empty();
        // 0x80 has 1 one (odd parity)
        flags.update_sz8(0x80);
        assert!(!flags.contains(Flags::PARITY));
    }

    // ---- Flags from_bits_truncate coverage ----

    #[test]
    fn test_flags_from_bits_truncate() {
        // from_bits_truncate should ignore unknown bits
        let flags = Flags::from_bits_truncate(0xFFFF);
        assert!(flags.contains(Flags::CARRY));
        assert!(flags.contains(Flags::ZERO));
        assert!(flags.contains(Flags::SIGN));
    }

    #[test]
    fn test_flags_bits_round_trip() {
        let flags = Flags::CARRY | Flags::ZERO | Flags::INTERRUPT | Flags::OVERFLOW;
        let bits = flags.bits();
        let restored = Flags::from_bits_truncate(bits);
        assert_eq!(flags, restored);
    }

    #[test]
    fn test_iopl_and_nested_flags() {
        let flags = Flags::IOPL0 | Flags::IOPL1 | Flags::NESTED;
        assert!(flags.contains(Flags::IOPL0));
        assert!(flags.contains(Flags::IOPL1));
        assert!(flags.contains(Flags::NESTED));
        assert_eq!(Flags::IOPL0.bits(), 0x1000);
        assert_eq!(Flags::IOPL1.bits(), 0x2000);
        assert_eq!(Flags::NESTED.bits(), 0x4000);
    }

    // ========================================================================
    // Mutation-testing-targeted tests
    //
    // These tests kill specific mutants identified by cargo-mutants on
    // flags.rs. Each targets a boundary condition or operator swap that
    // survived the existing test suite.
    // ========================================================================

    // --- Carry boundary: > vs >= (lines 93, 113, 150) ---
    // The carry flag check is `(a + b) > MAX`. If mutated to `>=`, the
    // boundary case where a+b == MAX exactly must NOT set carry.

    #[test]
    fn test_add8_carry_boundary_no_carry() {
        // 0x80 + 0x7F = 0xFF — exactly MAX, no carry
        let mut f = Flags::empty();
        f.update_add8(0x80, 0x7F, 0xFF);
        assert!(!f.contains(Flags::CARRY), "0x80+0x7F=0xFF should NOT carry");
    }

    #[test]
    fn test_add8_carry_boundary_yes_carry() {
        // 0x80 + 0x80 = 0x100 — exceeds MAX, carry
        let mut f = Flags::empty();
        f.update_add8(0x80, 0x80, 0x00);
        assert!(f.contains(Flags::CARRY), "0x80+0x80=0x100 should carry");
    }

    #[test]
    fn test_add16_carry_boundary_no_carry() {
        let mut f = Flags::empty();
        f.update_add16(0x8000, 0x7FFF, 0xFFFF);
        assert!(
            !f.contains(Flags::CARRY),
            "0x8000+0x7FFF=0xFFFF should NOT carry"
        );
    }

    #[test]
    fn test_add16_carry_boundary_yes_carry() {
        let mut f = Flags::empty();
        f.update_add16(0x8000, 0x8000, 0x0000);
        assert!(
            f.contains(Flags::CARRY),
            "0x8000+0x8000=0x10000 should carry"
        );
    }

    #[test]
    fn test_add32_carry_boundary_no_carry() {
        let mut f = Flags::empty();
        f.update_add32(0x8000_0000, 0x7FFF_FFFF, 0xFFFF_FFFF);
        assert!(!f.contains(Flags::CARRY));
    }

    #[test]
    fn test_add32_carry_boundary_yes_carry() {
        let mut f = Flags::empty();
        f.update_add32(0x8000_0000, 0x8000_0000, 0x0000_0000);
        assert!(f.contains(Flags::CARRY));
    }

    // --- Aux carry boundary: > vs >= (lines 95, 115, 155) ---

    #[test]
    fn test_add8_aux_carry_boundary_no_af() {
        // low nibbles: 0x08 + 0x07 = 0x0F — exactly MAX, no AF
        let mut f = Flags::empty();
        f.update_add8(0x08, 0x07, 0x0F);
        assert!(
            !f.contains(Flags::AUX_CARRY),
            "0x08+0x07 low nibble=0x0F, no AF"
        );
    }

    #[test]
    fn test_add8_aux_carry_boundary_yes_af() {
        // low nibbles: 0x08 + 0x08 = 0x10 — exceeds, AF
        let mut f = Flags::empty();
        f.update_add8(0x08, 0x08, 0x10);
        assert!(
            f.contains(Flags::AUX_CARRY),
            "0x08+0x08 low nibble=0x10, AF"
        );
    }

    #[test]
    fn test_add16_aux_carry_boundary_no_af() {
        let mut f = Flags::empty();
        f.update_add16(0x0008, 0x0007, 0x000F);
        assert!(!f.contains(Flags::AUX_CARRY));
    }

    #[test]
    fn test_add32_aux_carry_boundary_no_af() {
        let mut f = Flags::empty();
        f.update_add32(0x0000_0008, 0x0000_0007, 0x0000_000F);
        assert!(!f.contains(Flags::AUX_CARRY));
    }

    // --- Sub carry/borrow boundary: < vs <= (lines 133, 142, 161) ---

    #[test]
    fn test_sub8_borrow_boundary_no_carry() {
        // 5 - 5 = 0, a == b, no borrow
        let mut f = Flags::empty();
        f.update_sub8(5, 5, 0);
        assert!(!f.contains(Flags::CARRY), "5-5=0 should NOT borrow");
    }

    #[test]
    fn test_sub8_borrow_boundary_yes_carry() {
        // 4 - 5 = 0xFF, a < b, borrow
        let mut f = Flags::empty();
        f.update_sub8(4, 5, 0xFF);
        assert!(f.contains(Flags::CARRY), "4-5 should borrow");
    }

    #[test]
    fn test_sub16_borrow_boundary_no_carry() {
        let mut f = Flags::empty();
        f.update_sub16(100, 100, 0);
        assert!(!f.contains(Flags::CARRY));
    }

    #[test]
    fn test_sub32_borrow_boundary_no_carry() {
        let mut f = Flags::empty();
        f.update_sub32(100, 100, 0);
        assert!(!f.contains(Flags::CARRY));
    }

    // --- Sub aux carry boundary: < vs <= (lines 135, 144, 163) ---

    #[test]
    fn test_sub8_aux_borrow_boundary_no_af() {
        // low nibbles: 7 - 7 = 0, equal, no AF
        let mut f = Flags::empty();
        f.update_sub8(0x07, 0x07, 0x00);
        assert!(
            !f.contains(Flags::AUX_CARRY),
            "0x07-0x07 low nibble equal, no AF"
        );
    }

    #[test]
    fn test_sub8_aux_borrow_boundary_yes_af() {
        // low nibbles: 6 - 7 = borrow, AF
        let mut f = Flags::empty();
        f.update_sub8(0x06, 0x07, 0xFF);
        assert!(
            f.contains(Flags::AUX_CARRY),
            "0x06-0x07 low nibble borrow, AF"
        );
    }

    // --- ADC carry boundary: > vs >= (lines 103, 123) ---

    #[test]
    fn test_adc8_carry_boundary_no_carry() {
        // 0x80 + 0x7F + 0 = 0xFF — exactly MAX, no carry
        let mut f = Flags::empty();
        f.update_adc8(0x80, 0x7F, 0, 0xFF);
        assert!(!f.contains(Flags::CARRY));
    }

    #[test]
    fn test_adc8_carry_boundary_with_carry_in() {
        // 0x80 + 0x7F + 1 = 0x100 — carry
        let mut f = Flags::empty();
        f.update_adc8(0x80, 0x7F, 1, 0x00);
        assert!(f.contains(Flags::CARRY));
    }

    #[test]
    fn test_adc16_carry_boundary_no_carry() {
        let mut f = Flags::empty();
        f.update_adc16(0x8000, 0x7FFF, 0, 0xFFFF);
        assert!(!f.contains(Flags::CARRY));
    }

    // --- ADC aux carry boundary (line 106, 126) ---

    #[test]
    fn test_adc8_aux_carry_boundary() {
        // low nibbles: 0x07 + 0x08 + 0 = 0x0F — no AF
        let mut f = Flags::empty();
        f.update_adc8(0x07, 0x08, 0, 0x0F);
        assert!(!f.contains(Flags::AUX_CARRY));

        // low nibbles: 0x07 + 0x08 + 1 = 0x10 — AF
        f.update_adc8(0x07, 0x08, 1, 0x10);
        assert!(f.contains(Flags::AUX_CARRY));
    }

    #[test]
    fn test_adc16_aux_carry_boundary() {
        // This also catches `+ with -` on line 126: (a & 0xF) + (b & 0xF) + carry
        let mut f = Flags::empty();
        // 0x0007 + 0x0008 + 1 = low nibbles 7+8+1=16 > 15 → AF
        f.update_adc16(0x0007, 0x0008, 1, 0x0010);
        assert!(f.contains(Flags::AUX_CARRY));
        // 0x0007 + 0x0008 + 0 = low nibbles 7+8+0=15 → no AF
        f.update_adc16(0x0007, 0x0008, 0, 0x000F);
        assert!(!f.contains(Flags::AUX_CARRY));
    }

    // --- Overflow flag: ^ vs | and & vs | in OF expression ---
    // The overflow expression is: ((a ^ result) & (b ^ result) & SIGN_BIT) != 0
    // For ADD: overflow when both operands same sign but result different sign.
    // We need operand combinations where ^ and | produce different results.

    #[test]
    fn test_add8_overflow_positive_no_overflow() {
        // 0x40 + 0x01 = 0x41 — both positive, result positive, no OF
        // With ^ vs |: a^result=0x01, b^result=0x40 → &0x80=0 (both correct)
        // Need a case where swapping ^ with | gives different result.
        // 0x40 + 0x3F = 0x7F — positive+positive=positive, no OF
        let mut f = Flags::empty();
        f.update_add8(0x40, 0x3F, 0x7F);
        assert!(!f.contains(Flags::OVERFLOW));
    }

    #[test]
    fn test_add8_overflow_positive_overflow() {
        // 0x40 + 0x40 = 0x80 — positive+positive=negative, OF!
        let mut f = Flags::empty();
        f.update_add8(0x40, 0x40, 0x80);
        assert!(f.contains(Flags::OVERFLOW));
    }

    #[test]
    fn test_add8_overflow_mixed_signs_no_overflow() {
        // 0x80 + 0x01 = 0x81 — negative+positive=negative, no OF
        // (a ^ result) = 0x80^0x81=0x01, (b ^ result)=0x01^0x81=0x80
        // & 0x80 = 0x00 & 0x80 = 0 → no OF. Correct.
        // But with | instead of first &: 0x01 | 0x80 = 0x81 & 0x80 = 0x80 ≠ 0 → wrong OF!
        let mut f = Flags::empty();
        f.update_add8(0x80, 0x01, 0x81);
        assert!(
            !f.contains(Flags::OVERFLOW),
            "negative+positive must not overflow"
        );
    }

    #[test]
    fn test_add8_overflow_negative_no_overflow() {
        // 0xFF + 0x80 = 0x7F — negative+negative=positive... wait, that's overflow.
        // Actually: 0xFF(-1) + 0x80(-128) = -129 which overflows to 0x7F.
        // Let's use: 0xFF(-1) + 0xFF(-1) = 0xFE(-2), no overflow
        let mut f = Flags::empty();
        f.update_add8(0xFF, 0xFF, 0xFE);
        assert!(!f.contains(Flags::OVERFLOW));
    }

    #[test]
    fn test_add8_overflow_negative_overflow() {
        // 0x80(-128) + 0x80(-128) = 0x00 — negative+negative=zero(positive), OF!
        let mut f = Flags::empty();
        f.update_add8(0x80, 0x80, 0x00);
        assert!(f.contains(Flags::OVERFLOW));
    }

    // Repeat for 16/32-bit to catch those size-specific mutants

    #[test]
    fn test_add16_overflow_mixed_no_overflow() {
        let mut f = Flags::empty();
        f.update_add16(0x8000, 0x0001, 0x8001);
        assert!(!f.contains(Flags::OVERFLOW));
    }

    #[test]
    fn test_add16_overflow_positive_overflow() {
        let mut f = Flags::empty();
        f.update_add16(0x4000, 0x4000, 0x8000);
        assert!(f.contains(Flags::OVERFLOW));
    }

    #[test]
    fn test_add32_overflow_mixed_no_overflow() {
        let mut f = Flags::empty();
        f.update_add32(0x8000_0000, 0x0000_0001, 0x8000_0001);
        assert!(!f.contains(Flags::OVERFLOW));
    }

    #[test]
    fn test_add32_overflow_positive_overflow() {
        let mut f = Flags::empty();
        f.update_add32(0x4000_0000, 0x4000_0000, 0x8000_0000);
        assert!(f.contains(Flags::OVERFLOW));
    }

    // --- Sub overflow: ((a ^ b) & (a ^ result) & SIGN_BIT) ---
    // Overflow when subtracting operands of different signs gives wrong-sign result.

    #[test]
    fn test_sub8_overflow_mixed_no_overflow() {
        // 0x80(-128) - 0x01(1) = 0x7F(127) — different signs, result sign flip → OF!
        // Wait: 0x80 - 0x01 = 0x7F. a=neg, b=pos, result=pos. neg-pos=pos is wrong → OF
        let mut f = Flags::empty();
        f.update_sub8(0x80, 0x01, 0x7F);
        assert!(f.contains(Flags::OVERFLOW));
    }

    #[test]
    fn test_sub8_overflow_same_sign_no_overflow() {
        // 0x80(-128) - 0x80(-128) = 0x00 — same signs, no OF
        let mut f = Flags::empty();
        f.update_sub8(0x80, 0x80, 0x00);
        assert!(!f.contains(Flags::OVERFLOW));
    }

    #[test]
    fn test_sub8_no_overflow_pos_minus_pos() {
        // 0x40 - 0x01 = 0x3F — pos-pos=pos, no OF
        let mut f = Flags::empty();
        f.update_sub8(0x40, 0x01, 0x3F);
        assert!(!f.contains(Flags::OVERFLOW));
    }

    #[test]
    fn test_sub16_overflow_mixed() {
        let mut f = Flags::empty();
        f.update_sub16(0x8000, 0x0001, 0x7FFF);
        assert!(f.contains(Flags::OVERFLOW));
    }

    #[test]
    fn test_sub16_no_overflow_same_sign() {
        let mut f = Flags::empty();
        f.update_sub16(0x8000, 0x8000, 0x0000);
        assert!(!f.contains(Flags::OVERFLOW));
    }

    #[test]
    fn test_sub32_overflow_mixed() {
        let mut f = Flags::empty();
        f.update_sub32(0x8000_0000, 0x0000_0001, 0x7FFF_FFFF);
        assert!(f.contains(Flags::OVERFLOW));
    }

    #[test]
    fn test_sub32_no_overflow_same_sign() {
        let mut f = Flags::empty();
        f.update_sub32(0x8000_0000, 0x8000_0000, 0x0000_0000);
        assert!(!f.contains(Flags::OVERFLOW));
    }

    // --- ADC/SBB overflow tests ---

    #[test]
    fn test_adc8_overflow_mixed_no_overflow() {
        let mut f = Flags::empty();
        f.update_adc8(0x80, 0x01, 0, 0x81);
        assert!(!f.contains(Flags::OVERFLOW));
    }

    #[test]
    fn test_adc16_overflow_positive_overflow() {
        let mut f = Flags::empty();
        f.update_adc16(0x4000, 0x4000, 0, 0x8000);
        assert!(f.contains(Flags::OVERFLOW));
    }

    #[test]
    fn test_adc16_overflow_mixed_no_overflow() {
        let mut f = Flags::empty();
        f.update_adc16(0x8000, 0x0001, 0, 0x8001);
        assert!(!f.contains(Flags::OVERFLOW));
    }

    // --- SBB32 carry/overflow boundary tests ---

    #[test]
    fn test_sbb32_carry_boundary_no_borrow() {
        let mut f = Flags::empty();
        // 100 - 100 - 0 = 0, no borrow
        f.update_sbb32(100, 100, 0, 0);
        assert!(!f.contains(Flags::CARRY));
    }

    #[test]
    fn test_sbb32_overflow_mixed() {
        let mut f = Flags::empty();
        // 0x80000000 - 1 - 0 = 0x7FFFFFFF → neg - pos = pos → OF
        f.update_sbb32(0x8000_0000, 1, 0, 0x7FFF_FFFF);
        assert!(f.contains(Flags::OVERFLOW));
    }

    #[test]
    fn test_sbb32_no_overflow_same_sign() {
        let mut f = Flags::empty();
        f.update_sbb32(0x8000_0000, 0x8000_0000, 0, 0);
        assert!(!f.contains(Flags::OVERFLOW));
    }

    // ========================================================================
    // Mutant-killing round 2: target remaining `^ with |` in OF expressions
    // and missing boundary tests for 32-bit/sub/sbb/adc functions.
    // ========================================================================

    // --- OF expression `(b ^ result)` vs `(b | result)` for sub/32-bit ---
    // sub OF expression: ((a ^ b) & (a ^ result) & SIGN) != 0
    // Need a=neg, b=neg, result=neg: same sign operands, same sign result → no OF
    // a^b has sign bit clear (same sign), but a|b has sign bit set.

    #[test]
    fn test_sub8_of_expression_a_xor_vs_or() {
        // a=0x80, b=0x81, result=0xFF. a^b=0x01(bit7=0), a|b=0x81(bit7=1)
        let mut f = Flags::empty();
        f.update_sub8(0x80, 0x81, 0xFF);
        assert!(!f.contains(Flags::OVERFLOW));
    }

    #[test]
    fn test_sub16_of_expression_a_xor_vs_or() {
        let mut f = Flags::empty();
        f.update_sub16(0x8000, 0x8001, 0xFFFF);
        assert!(!f.contains(Flags::OVERFLOW));
    }

    #[test]
    fn test_sub32_of_expression_a_xor_vs_or() {
        let mut f = Flags::empty();
        f.update_sub32(0x8000_0000, 0x8000_0001, 0xFFFF_FFFF);
        assert!(!f.contains(Flags::OVERFLOW));
    }

    #[test]
    fn test_add32_of_expression_b_xor_vs_or() {
        // a=pos, b=neg, result=neg → no OF
        let mut f = Flags::empty();
        f.update_add32(0x0000_0001, 0x8000_0000, 0x8000_0001);
        assert!(!f.contains(Flags::OVERFLOW));
    }

    #[test]
    fn test_adc32_of_expression_b_xor_vs_or() {
        let mut f = Flags::empty();
        f.update_adc32(0x0000_0001, 0x8000_0000, 0, 0x8000_0001);
        assert!(!f.contains(Flags::OVERFLOW));
    }

    #[test]
    fn test_sbb32_of_expression_a_xor_vs_or() {
        let mut f = Flags::empty();
        f.update_sbb32(0x8000_0000, 0x8000_0001, 0, 0xFFFF_FFFF);
        assert!(!f.contains(Flags::OVERFLOW));
    }

    // --- adc32 carry/aux boundary (lines 171, 177) ---

    #[test]
    fn test_adc32_carry_boundary_no_carry() {
        let mut f = Flags::empty();
        f.update_adc32(0x8000_0000, 0x7FFF_FFFF, 0, 0xFFFF_FFFF);
        assert!(!f.contains(Flags::CARRY));
    }

    #[test]
    fn test_adc32_carry_boundary_yes_carry() {
        let mut f = Flags::empty();
        f.update_adc32(0x8000_0000, 0x7FFF_FFFF, 1, 0x0000_0000);
        assert!(f.contains(Flags::CARRY));
    }

    #[test]
    fn test_adc32_aux_carry_boundary() {
        let mut f = Flags::empty();
        // 7 + 8 + 0 = 15 → no AF
        f.update_adc32(0x07, 0x08, 0, 0x0F);
        assert!(!f.contains(Flags::AUX_CARRY));
        // 7 + 8 + 1 = 16 → AF
        f.update_adc32(0x07, 0x08, 1, 0x10);
        assert!(f.contains(Flags::AUX_CARRY));
    }

    // --- sub16/sub32 aux carry boundary (lines 144, 163) ---

    #[test]
    fn test_sub16_aux_borrow_boundary_no_af() {
        let mut f = Flags::empty();
        f.update_sub16(0x0007, 0x0007, 0x0000);
        assert!(!f.contains(Flags::AUX_CARRY));
    }

    #[test]
    fn test_sub16_aux_borrow_boundary_yes_af() {
        let mut f = Flags::empty();
        f.update_sub16(0x0006, 0x0007, 0xFFFF);
        assert!(f.contains(Flags::AUX_CARRY));
    }

    #[test]
    fn test_sub32_aux_borrow_boundary_no_af() {
        let mut f = Flags::empty();
        f.update_sub32(0x07, 0x07, 0x00);
        assert!(!f.contains(Flags::AUX_CARRY));
    }

    #[test]
    fn test_sub32_aux_borrow_boundary_yes_af() {
        let mut f = Flags::empty();
        f.update_sub32(0x06, 0x07, 0xFFFF_FFFF);
        assert!(f.contains(Flags::AUX_CARRY));
    }

    // --- sbb32 aux carry boundary (line 188: (a & 0xF) < (b & 0xF) + borrow) ---

    #[test]
    fn test_sbb32_aux_borrow_boundary() {
        let mut f = Flags::empty();
        // a_low=7, b_low=7, borrow=0 → 7 < 7+0=7 → false (no AF)
        f.update_sbb32(0x07, 0x07, 0, 0x00);
        assert!(!f.contains(Flags::AUX_CARRY));
        // a_low=7, b_low=7, borrow=1 → 7 < 7+1=8 → true (AF)
        f.update_sbb32(0x07, 0x07, 1, 0xFFFF_FFFF);
        assert!(f.contains(Flags::AUX_CARRY));
    }

    // ========================================================================
    // Mutant-killing round 3: target (b ^ result) → (b | result) at col 54
    // in the OF expression across ALL add/adc/sub/sbb size variants.
    //
    // The second ^ in ((a ^ result) & (b ^ result) & SIGN_BIT) is at col 54.
    // To distinguish ^ from |: need b and result both with sign bit SET.
    // Then b^result sign=0, b|result sign=1.
    // Combined with (a^result) having sign=1 (a different sign from result):
    //   a=positive, b=negative, result=negative → no real overflow.
    // ========================================================================

    #[test]
    fn test_add8_of_col54_b_xor_result() {
        let mut f = Flags::empty();
        f.update_add8(0x01, 0x80, 0x81); // pos+neg=neg, no OF
        assert!(!f.contains(Flags::OVERFLOW));
    }

    #[test]
    fn test_adc8_of_col54_b_xor_result() {
        let mut f = Flags::empty();
        f.update_adc8(0x01, 0x80, 0, 0x81);
        assert!(!f.contains(Flags::OVERFLOW));
    }

    #[test]
    fn test_add16_of_col54_b_xor_result() {
        let mut f = Flags::empty();
        f.update_add16(0x0001, 0x8000, 0x8001);
        assert!(!f.contains(Flags::OVERFLOW));
    }

    #[test]
    fn test_adc16_of_col54_b_xor_result() {
        let mut f = Flags::empty();
        f.update_adc16(0x0001, 0x8000, 0, 0x8001);
        assert!(!f.contains(Flags::OVERFLOW));
    }

    #[test]
    fn test_adc32_of_col54_b_xor_result() {
        let mut f = Flags::empty();
        f.update_adc32(0x0000_0001, 0x8000_0000, 0, 0x8000_0001);
        assert!(!f.contains(Flags::OVERFLOW));
    }

    // Sub expression: ((a ^ b) & (a ^ result) & SIGN) — the second ^ is (a ^ result).
    // To distinguish: a and result both with sign bit SET.
    // a=negative, b=negative, result=negative → no real overflow.
    // a^result sign=0, a|result sign=1.
    // Combined with (a^b) having sign=0 (same sign):
    //   This already gives OF=0 with both ^ and |. Need different approach.
    // For SUB: need a^b sign=1 AND a^result sign=0 (both a,result negative).
    //   a=neg, b=pos → a^b sign=1. result=neg → a^result sign=0.
    //   Original: 1 & 0 & sign = 0. Mutated: 1 & 1 & sign = 1 (false OF!)
    // a=0x80, b=0x01, result=0x7F? No, 0x80-0x01=0x7F is positive. Not what we want.
    // a=0xFF(-1), b=0x7E(+126), result=0x81(-127). a^b=0x81(sign=1), a^result=0x7E(sign=0).
    // a|result=0xFF(sign=1).
    // Original: 0x81 & 0x7E & 0x80 = 0x00 (no OF) ✓
    // Mutated:  0x81 & 0xFF & 0x80 = 0x80 (false OF!) ✗
    // So: update_sub8(0xFF, 0x7E, 0x81) should catch it.

    #[test]
    fn test_sub8_of_col49_a_xor_result() {
        let mut f = Flags::empty();
        f.update_sub8(0xFF, 0x7E, 0x81); // neg-pos=neg, no OF
        assert!(!f.contains(Flags::OVERFLOW));
    }

    #[test]
    fn test_sub16_of_col49_a_xor_result() {
        let mut f = Flags::empty();
        f.update_sub16(0xFFFF, 0x7FFE, 0x8001);
        assert!(!f.contains(Flags::OVERFLOW));
    }

    #[test]
    fn test_sub32_of_col49_a_xor_result() {
        let mut f = Flags::empty();
        f.update_sub32(0xFFFF_FFFF, 0x7FFF_FFFE, 0x8000_0001);
        assert!(!f.contains(Flags::OVERFLOW));
    }

    #[test]
    fn test_sbb32_of_col49_a_xor_result() {
        let mut f = Flags::empty();
        f.update_sbb32(0xFFFF_FFFF, 0x7FFF_FFFE, 0, 0x8000_0001);
        assert!(!f.contains(Flags::OVERFLOW));
    }
}
