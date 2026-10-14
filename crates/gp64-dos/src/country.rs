//! DOS country/locale information.
//!
//! Provides the `DosCountryInfo` struct and host locale detection for INT 21h
//! functions AH=38h (Get/Set Country Info), AH=65h (Get Extended Country Info),
//! and AH=66h (Get/Set Global Code Page).

use gp64_memory::Memory;

/// DOS country information selected by the DOS country code.
///
/// This struct represents the data returned by INT 21h AH=38h (34-byte buffer)
/// and INT 21h AH=65h AL=01h (41-byte extended buffer). Both share the same
/// core formatting fields; the extended version adds a header (info ID, size)
/// and the country code / code page.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DosCountryInfo {
    /// DOS country code (1=USA, 44=UK, 49=Germany, etc.)
    pub country_code: u16,
    /// Active code page (437=US, 850=Multilingual Latin I, etc.)
    pub code_page: u16,
    /// Date format: 0=MDY, 1=DMY, 2=YMD
    pub date_format: u16,
    /// Currency symbol (up to 4 bytes + null terminator, stored as 5-byte array)
    pub currency_symbol: [u8; 5],
    /// Thousands separator character
    pub thousands_separator: u8,
    /// Decimal separator character
    pub decimal_separator: u8,
    /// Date separator character
    pub date_separator: u8,
    /// Time separator character
    pub time_separator: u8,
    /// Currency format flags:
    ///   0 = symbol precedes, no space ($1.00)
    ///   1 = symbol follows, no space (1.00$)
    ///   2 = symbol precedes, with space ($ 1.00)
    ///   3 = symbol follows, with space (1.00 $)
    pub currency_format: u8,
    /// Number of decimal places in currency (typically 0 or 2)
    pub currency_decimal_places: u8,
    /// Time format: 0=12-hour, 1=24-hour
    pub time_format: u8,
    /// Data list separator (used in DATA statements, CSV-like contexts)
    pub data_list_separator: u8,
}

impl DosCountryInfo {
    /// US defaults (used as fallback and in tests).
    pub fn us() -> Self {
        Self {
            country_code: 1,
            code_page: 437,
            date_format: 0,
            currency_symbol: [b'$', 0, 0, 0, 0],
            thousands_separator: b',',
            decimal_separator: b'.',
            date_separator: b'-',
            time_separator: b':',
            currency_format: 0,
            currency_decimal_places: 2,
            time_format: 0,
            data_list_separator: b',',
        }
    }

    /// Write the 34-byte country info buffer (for AH=38h GET).
    ///
    /// Buffer layout (offsets in hex):
    /// ```text
    /// 00h  2  Date format (0=MDY, 1=DMY, 2=YMD)
    /// 02h  5  Currency symbol (ASCIIZ)
    /// 07h  2  Thousands separator (ASCIIZ)
    /// 09h  2  Decimal separator (ASCIIZ)
    /// 0Bh  2  Date separator (ASCIIZ)
    /// 0Dh  2  Time separator (ASCIIZ)
    /// 0Fh  1  Currency format
    /// 10h  1  Digits after decimal in currency
    /// 11h  1  Time format (0=12hr, 1=24hr)
    /// 12h  4  Case map call address (far pointer)
    /// 16h  2  Data list separator (ASCIIZ)
    /// 18h 10  Reserved (zeros)
    /// ```
    pub fn write_to_buffer(&self, memory: &mut Memory, addr: usize) {
        memory.write_u16(addr, self.date_format);

        // Currency symbol (5 bytes at offset 02h)
        for (i, &b) in self.currency_symbol.iter().enumerate() {
            memory.write_u8(addr + 0x02 + i, b);
        }

        // Thousands separator (2 bytes ASCIIZ)
        memory.write_u8(addr + 0x07, self.thousands_separator);
        memory.write_u8(addr + 0x08, 0);

        // Decimal separator
        memory.write_u8(addr + 0x09, self.decimal_separator);
        memory.write_u8(addr + 0x0A, 0);

        // Date separator
        memory.write_u8(addr + 0x0B, self.date_separator);
        memory.write_u8(addr + 0x0C, 0);

        // Time separator
        memory.write_u8(addr + 0x0D, self.time_separator);
        memory.write_u8(addr + 0x0E, 0);

        memory.write_u8(addr + 0x0F, self.currency_format);
        memory.write_u8(addr + 0x10, self.currency_decimal_places);
        memory.write_u8(addr + 0x11, self.time_format);

        // Case map call address (dummy far pointer)
        memory.write_u32(addr + 0x12, 0xFFFF0000);

        // Data list separator
        memory.write_u8(addr + 0x16, self.data_list_separator);
        memory.write_u8(addr + 0x17, 0);

        // Reserved (10 bytes: 0x18..0x22)
        for i in 0x18..0x22 {
            memory.write_u8(addr + i, 0);
        }
    }

    /// Write the 41-byte extended country info buffer (for AH=65h AL=01h).
    ///
    /// The extended format prepends a 7-byte header to the 34-byte AH=38h layout:
    /// ```text
    /// 00h  1  Info ID (01h)
    /// 01h  2  Size of following data (38)
    /// 03h  2  Country code
    /// 05h  2  Code page
    /// 07h 34  Same as AH=38h buffer
    /// ```
    pub fn write_extended_to_buffer(&self, memory: &mut Memory, addr: usize) {
        memory.write_u8(addr, 0x01); // Info ID
        memory.write_u16(addr + 0x01, 38); // Size of following data
        memory.write_u16(addr + 0x03, self.country_code);
        memory.write_u16(addr + 0x05, self.code_page);
        // Remaining 34 bytes are identical to the AH=38h format
        self.write_to_buffer(memory, addr + 0x07);
    }

    /// Serialize the 41-byte extended country info to a byte array (for
    /// buffer-size-limited writes in AH=65h).
    pub fn extended_to_bytes(&self) -> [u8; 41] {
        let mut buf = [0u8; 41];
        buf[0] = 0x01; // Info ID
        buf[1..3].copy_from_slice(&38u16.to_le_bytes()); // Size of following data
        buf[3..5].copy_from_slice(&self.country_code.to_le_bytes());
        buf[5..7].copy_from_slice(&self.code_page.to_le_bytes());
        // 34-byte AH=38h body at offset 7
        let b = &mut buf[7..];
        b[0..2].copy_from_slice(&self.date_format.to_le_bytes());
        b[2..7].copy_from_slice(&self.currency_symbol);
        b[7] = self.thousands_separator;
        b[8] = 0;
        b[9] = self.decimal_separator;
        b[10] = 0;
        b[11] = self.date_separator;
        b[12] = 0;
        b[13] = self.time_separator;
        b[14] = 0;
        b[15] = self.currency_format;
        b[16] = self.currency_decimal_places;
        b[17] = self.time_format;
        // Case map call address (dummy far pointer)
        b[18..22].copy_from_slice(&0xFFFF0000u32.to_le_bytes());
        b[22] = self.data_list_separator;
        b[23] = 0;
        // Reserved (10 bytes: 0x18..0x22 relative to body = indices 24..34)
        // Already zeroed by array init
        buf
    }
}

// ── Country Table ──────────────────────────────────────────────────────

/// Internal table entry for DOS country definitions.
/// Verified against MS-DOS 6.22 COUNTRY help and the DOS model.
struct CountryTableEntry {
    dos_code: u16,
    #[cfg_attr(not(test), allow(dead_code))] // ISO codes are checked by table tests
    iso_alpha2: &'static str,
    date_format: u16,
    currency: &'static [u8],
    thousands_sep: u8,
    decimal_sep: u8,
    date_sep: u8,
    time_sep: u8,
    currency_format: u8,
    currency_decimals: u8,
    time_format: u8,
    list_sep: u8,
    code_page: u16,
}

impl CountryTableEntry {
    fn to_country_info(&self) -> DosCountryInfo {
        let mut currency_symbol = [0u8; 5];
        let len = self.currency.len().min(4);
        currency_symbol[..len].copy_from_slice(&self.currency[..len]);

        DosCountryInfo {
            country_code: self.dos_code,
            code_page: self.code_page,
            date_format: self.date_format,
            currency_symbol,
            thousands_separator: self.thousands_sep,
            decimal_separator: self.decimal_sep,
            date_separator: self.date_sep,
            time_separator: self.time_sep,
            currency_format: self.currency_format,
            currency_decimal_places: self.currency_decimals,
            time_format: self.time_format,
            data_list_separator: self.list_sep,
        }
    }
}

/// Helper to construct a `CountryTableEntry` (reduces visual noise in the table).
const fn cte(
    dos_code: u16,
    iso_alpha2: &'static str,
    date_format: u16,
    currency: &'static [u8],
    thousands_sep: u8,
    decimal_sep: u8,
    date_sep: u8,
    time_sep: u8,
    currency_format: u8,
    currency_decimals: u8,
    time_format: u8,
    list_sep: u8,
    code_page: u16,
) -> CountryTableEntry {
    CountryTableEntry {
        dos_code,
        iso_alpha2,
        date_format,
        currency,
        thousands_sep,
        decimal_sep,
        date_sep,
        time_sep,
        currency_format,
        currency_decimals,
        time_format,
        list_sep,
        code_page,
    }
}

#[rustfmt::skip]
static COUNTRY_TABLE: &[CountryTableEntry] = &[
    // Code  ISO    Fmt  Currency  1000s  Dec    Date   Time  CF CD TF List  CP
    // ─── North America ─────────────────────────────────────────────────────
    cte(1,   "US",  0, b"$",     b',', b'.', b'-', b':', 0, 2, 0, b',', 437),  // United States
    cte(2,   "CA",  2, b"$",     b',', b'.', b'-', b':', 0, 2, 1, b',', 863),  // Canada (French)
    cte(3,   "XX",  1, b"$",     b',', b'.', b'/', b':', 0, 2, 0, b',', 850),  // Latin America
    cte(52,  "MX",  1, b"$",     b',', b'.', b'/', b':', 0, 2, 1, b',', 850),  // Mexico
    // ─── South America ────────────────────────────────────────────────────
    cte(51,  "PE",  1, b"S/",    b',', b'.', b'/', b':', 0, 2, 0, b',', 850),  // Peru
    cte(54,  "AR",  1, b"$",     b'.', b',', b'/', b':', 0, 2, 1, b',', 850),  // Argentina
    cte(55,  "BR",  1, b"R$",    b'.', b',', b'/', b':', 0, 2, 1, b';', 850),  // Brazil
    cte(56,  "CL",  1, b"$",     b'.', b',', b'/', b':', 0, 2, 1, b';', 850),  // Chile
    cte(57,  "CO",  1, b"$",     b'.', b',', b'/', b':', 0, 2, 0, b',', 850),  // Colombia
    cte(58,  "VE",  1, b"Bs",    b'.', b',', b'/', b':', 0, 2, 0, b',', 850),  // Venezuela
    // ─── Western Europe ───────────────────────────────────────────────────
    cte(31,  "NL",  1, b"E",     b'.', b',', b'-', b':', 2, 2, 1, b';', 850),  // Netherlands
    cte(32,  "BE",  1, b"E",     b'.', b',', b'/', b':', 3, 2, 1, b';', 850),  // Belgium
    cte(33,  "FR",  1, b"E",     b' ', b',', b'/', b':', 3, 2, 1, b';', 850),  // France
    cte(34,  "ES",  1, b"E",     b'.', b',', b'/', b':', 3, 2, 1, b';', 850),  // Spain
    cte(39,  "IT",  1, b"E",     b'.', b',', b'/', b'.', 2, 2, 1, b';', 850),  // Italy
    cte(41,  "CH",  1, b"Fr.",   b'\'',b'.', b'.', b':', 2, 2, 1, b';', 850),  // Switzerland
    cte(43,  "AT",  1, b"E",     b'.', b',', b'.', b':', 3, 2, 1, b';', 850),  // Austria
    cte(44,  "GB",  1, b"\x9C",  b',', b'.', b'/', b':', 0, 2, 1, b',', 850),  // UK (£=0x9C)
    cte(49,  "DE",  1, b"E",     b'.', b',', b'.', b':', 3, 2, 1, b';', 850),  // Germany
    cte(351, "PT",  1, b"E",     b'.', b',', b'-', b':', 3, 2, 1, b';', 850),  // Portugal
    cte(353, "IE",  1, b"E",     b',', b'.', b'/', b':', 0, 2, 1, b',', 850),  // Ireland
    // ─── Northern Europe ──────────────────────────────────────────────────
    cte(45,  "DK",  1, b"kr",    b'.', b',', b'-', b'.', 2, 2, 1, b';', 865),  // Denmark
    cte(46,  "SE",  2, b"kr",    b' ', b',', b'-', b'.', 3, 2, 1, b';', 850),  // Sweden
    cte(47,  "NO",  1, b"kr",    b' ', b',', b'.', b':', 2, 2, 1, b';', 865),  // Norway
    cte(354, "IS",  1, b"kr",    b'.', b',', b'/', b':', 3, 0, 1, b';', 850),  // Iceland
    cte(358, "FI",  1, b"E",     b' ', b',', b'.', b'.', 3, 2, 1, b';', 850),  // Finland
    // ─── Eastern Europe ───────────────────────────────────────────────────
    cte(7,   "RU",  1, b"R",     b' ', b',', b'.', b':', 3, 2, 1, b';', 866),  // Russia
    cte(36,  "HU",  2, b"Ft",    b' ', b',', b'.', b':', 3, 2, 1, b';', 852),  // Hungary
    cte(38,  "RS",  1, b"Din",   b'.', b',', b'.', b':', 3, 2, 1, b';', 852),  // Yugoslavia/Serbia
    cte(40,  "RO",  1, b"Lei",   b'.', b',', b'.', b':', 3, 2, 1, b',', 852),  // Romania
    cte(42,  "CZ",  1, b"Kc",    b' ', b',', b'.', b':', 3, 2, 1, b';', 852),  // Czech Republic
    cte(48,  "PL",  1, b"zl",    b' ', b',', b'.', b':', 3, 2, 1, b';', 852),  // Poland
    cte(421, "SK",  1, b"Sk",    b' ', b',', b'.', b':', 3, 2, 1, b';', 852),  // Slovakia
    // ─── Southern Europe / Mediterranean ──────────────────────────────────
    cte(30,  "GR",  1, b"E",     b'.', b',', b'/', b':', 3, 2, 1, b',', 737),  // Greece
    cte(90,  "TR",  1, b"TL",    b'.', b',', b'/', b':', 3, 2, 1, b';', 857),  // Turkey
    cte(972, "IL",  1, b"NIS",   b',', b'.', b'/', b':', 2, 2, 1, b',', 862),  // Israel
    // ─── Asia-Pacific ─────────────────────────────────────────────────────
    cte(60,  "MY",  0, b"$",     b',', b'.', b'/', b':', 0, 2, 0, b',', 437),  // Malaysia
    cte(65,  "SG",  0, b"$",     b',', b'.', b'/', b':', 0, 2, 1, b',', 437),  // Singapore
    cte(81,  "JP",  2, b"\x5C",  b',', b'.', b'/', b':', 0, 0, 1, b',', 932),  // Japan (¥=0x5C)
    cte(82,  "KR",  2, b"W",     b',', b'.', b'.', b':', 0, 0, 1, b',', 949),  // South Korea
    cte(84,  "VN",  1, b"D",     b'.', b',', b'/', b':', 3, 2, 1, b',', 437),  // Vietnam
    cte(86,  "CN",  2, b"Y",     b',', b'.', b'.', b':', 0, 2, 1, b',', 936),  // China
    cte(91,  "IN",  1, b"Rs",    b',', b'.', b'/', b':', 0, 2, 1, b',', 437),  // India
    cte(886, "TW",  2, b"NT$",   b',', b'.', b'/', b':', 0, 2, 1, b',', 950),  // Taiwan
    // ─── Oceania ──────────────────────────────────────────────────────────
    cte(61,  "AU",  1, b"$",     b',', b'.', b'/', b':', 0, 2, 1, b',', 850),  // Australia
    cte(64,  "NZ",  1, b"$",     b',', b'.', b'/', b':', 0, 2, 1, b',', 850),  // New Zealand
    // ─── Africa ───────────────────────────────────────────────────────────
    cte(27,  "ZA",  1, b"R",     b',', b'.', b'/', b':', 0, 2, 1, b',', 850),  // South Africa
];

/// Look up a DOS country code in the static table.
pub fn country_from_code(code: u16) -> Option<DosCountryInfo> {
    COUNTRY_TABLE
        .iter()
        .find(|e| e.dos_code == code)
        .map(|e| e.to_country_info())
}

#[cfg(test)]
fn iso_alpha2_to_dos_code(iso: &str) -> Option<u16> {
    COUNTRY_TABLE
        .iter()
        .find(|e| e.iso_alpha2.eq_ignore_ascii_case(iso))
        .map(|e| e.dos_code)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_country_table_validity() {
        for entry in COUNTRY_TABLE {
            assert!(entry.date_format <= 2);
            assert!(entry.time_format <= 1);
            assert!(entry.thousands_sep > 0);
            assert!(entry.decimal_sep > 0);
            assert!(entry.date_sep > 0);
            assert!(entry.time_sep > 0);
            assert!(!entry.currency.is_empty());
            assert!(entry.currency.len() <= 4);
            assert!(entry.code_page > 0);
            assert!(entry.currency_format <= 3);
        }
    }

    #[test]
    fn test_country_table_no_duplicate_codes() {
        let mut seen = std::collections::HashSet::new();
        for entry in COUNTRY_TABLE {
            assert!(seen.insert(entry.dos_code));
        }
    }

    #[test]
    fn test_cte_builder_assigns_all_fields() {
        let entry = cte(
            999, "ZZ", 2, b"ABC", b'\'', b'.', b'/', b':', 3, 1, 1, b';', 850,
        );
        assert_eq!(entry.dos_code, 999);
        assert_eq!(entry.iso_alpha2, "ZZ");
        assert_eq!(entry.date_format, 2);
        assert_eq!(entry.currency, b"ABC");
        assert_eq!(entry.thousands_sep, b'\'');
        assert_eq!(entry.decimal_sep, b'.');
        assert_eq!(entry.date_sep, b'/');
        assert_eq!(entry.time_sep, b':');
        assert_eq!(entry.currency_format, 3);
        assert_eq!(entry.currency_decimals, 1);
        assert_eq!(entry.time_format, 1);
        assert_eq!(entry.list_sep, b';');
        assert_eq!(entry.code_page, 850);
    }

    #[test]
    fn test_us_defaults() {
        let us = DosCountryInfo::us();
        assert_eq!(us.country_code, 1);
        assert_eq!(us.code_page, 437);
        assert_eq!(us.date_format, 0);
        assert_eq!(us.currency_symbol[0], b'$');
        assert_eq!(us.time_format, 0);
        assert_eq!(us.thousands_separator, b',');
        assert_eq!(us.decimal_separator, b'.');
        assert_eq!(us.date_separator, b'-');
        assert_eq!(us.data_list_separator, b',');
    }

    #[test]
    fn test_us_from_table_matches_defaults() {
        let table_us = country_from_code(1).unwrap();
        let default_us = DosCountryInfo::us();
        assert_eq!(table_us, default_us);
    }

    #[test]
    fn test_country_from_code_germany() {
        let de = country_from_code(49).unwrap();
        assert_eq!(de.date_format, 1); // DMY
        assert_eq!(de.decimal_separator, b',');
        assert_eq!(de.thousands_separator, b'.');
        assert_eq!(de.date_separator, b'.');
        assert_eq!(de.time_format, 1); // 24-hour
        assert_eq!(de.code_page, 850);
        assert_eq!(de.data_list_separator, b';');
    }

    #[test]
    fn test_country_from_code_japan_ymd() {
        let jp = country_from_code(81).unwrap();
        assert_eq!(jp.date_format, 2); // YMD
        assert_eq!(jp.code_page, 932);
    }

    #[test]
    fn test_country_from_code_canada_french() {
        let ca = country_from_code(2).unwrap();
        assert_eq!(ca.date_format, 2); // YMD per MS-DOS 6.22
        assert_eq!(ca.time_format, 1); // 24-hour
        assert_eq!(ca.code_page, 863);
    }

    #[test]
    fn test_country_from_code_unknown() {
        assert!(country_from_code(9999).is_none());
        assert!(country_from_code(0).is_none());
    }

    #[test]
    fn test_iso_alpha2_to_dos_code() {
        assert_eq!(iso_alpha2_to_dos_code("DE"), Some(49));
        assert_eq!(iso_alpha2_to_dos_code("de"), Some(49)); // case-insensitive
        assert_eq!(iso_alpha2_to_dos_code("GB"), Some(44));
        assert_eq!(iso_alpha2_to_dos_code("US"), Some(1));
        assert_eq!(iso_alpha2_to_dos_code("ZZ"), None);
    }

    #[test]
    fn test_write_country_buffer() {
        let info = country_from_code(49).unwrap(); // Germany
        let mut memory = Memory::new();
        let addr = 0x1000;

        info.write_to_buffer(&mut memory, addr);

        // Date format = 1 (DMY)
        assert_eq!(memory.read_u16(addr), 1);
        // Currency = "E"
        assert_eq!(memory.read_u8(addr + 0x02), b'E');
        // Thousands = '.'
        assert_eq!(memory.read_u8(addr + 0x07), b'.');
        // Decimal = ','
        assert_eq!(memory.read_u8(addr + 0x09), b',');
        // Date sep = '.'
        assert_eq!(memory.read_u8(addr + 0x0B), b'.');
        // Time sep = ':'
        assert_eq!(memory.read_u8(addr + 0x0D), b':');
        // Currency format = 3
        assert_eq!(memory.read_u8(addr + 0x0F), 3);
        // Time format = 1 (24-hour)
        assert_eq!(memory.read_u8(addr + 0x11), 1);
        // Case map (dummy)
        assert_eq!(memory.read_u32(addr + 0x12), 0xFFFF0000);
        // List sep = ';'
        assert_eq!(memory.read_u8(addr + 0x16), b';');
        // Reserved bytes should be zero
        assert_eq!(memory.read_u8(addr + 0x18), 0);
        assert_eq!(memory.read_u8(addr + 0x21), 0);
    }

    #[test]
    fn test_write_extended_buffer() {
        let info = country_from_code(44).unwrap(); // UK
        let mut memory = Memory::new();
        let addr = 0x2000;

        info.write_extended_to_buffer(&mut memory, addr);

        // Info ID
        assert_eq!(memory.read_u8(addr), 0x01);
        // Size
        assert_eq!(memory.read_u16(addr + 0x01), 38);
        // Country code
        assert_eq!(memory.read_u16(addr + 0x03), 44);
        // Code page
        assert_eq!(memory.read_u16(addr + 0x05), 850);
        // Date format (at header offset + 0x07)
        assert_eq!(memory.read_u16(addr + 0x07), 1); // DMY
                                                     // Currency (£ = 0x9C in CP850)
        assert_eq!(memory.read_u8(addr + 0x09), 0x9C);
    }

    /// Verify `extended_to_bytes()` produces identical output to `write_extended_to_buffer()`.
    #[test]
    fn test_extended_to_bytes_matches_memory_write() {
        let info = country_from_code(1).unwrap(); // USA
        let mut memory = Memory::new();
        let addr = 0x4000;
        info.write_extended_to_buffer(&mut memory, addr);

        let bytes = info.extended_to_bytes();
        for i in 0..41 {
            assert_eq!(bytes[i], memory.read_u8(addr + i), "mismatch at byte {i}");
        }
    }

    #[test]
    fn test_default_country_is_us() {
        let info = DosCountryInfo::us();
        assert_eq!(info.country_code, 1);
        assert!(info.date_format <= 2);
        assert!(info.time_format <= 1);
        assert!(info.currency_symbol[0] != 0);
        assert!(info.code_page > 0);
        assert!(info.currency_format <= 3);
    }

    // ========== Additional coverage tests ==========

    #[test]
    fn test_country_from_code_uk() {
        let uk = country_from_code(44).unwrap();
        assert_eq!(uk.date_format, 1); // DMY
        assert_eq!(uk.thousands_separator, b',');
        assert_eq!(uk.decimal_separator, b'.');
    }

    #[test]
    fn test_country_from_code_france() {
        let fr = country_from_code(33).unwrap();
        assert_eq!(fr.date_format, 1); // DMY
        assert_eq!(fr.time_format, 1); // 24-hour
    }

    #[test]
    fn test_country_from_code_australia() {
        let au = country_from_code(61).unwrap();
        assert_eq!(au.date_format, 1); // DMY
    }

    #[test]
    fn test_country_info_partial_eq() {
        let us1 = DosCountryInfo::us();
        let us2 = DosCountryInfo::us();
        assert_eq!(us1, us2);

        let de = country_from_code(49).unwrap();
        assert_ne!(us1, de);
    }

    #[test]
    fn test_iso_alpha2_lookup_all_table_entries() {
        // Every entry in the table should be findable by its ISO code
        for entry in COUNTRY_TABLE {
            let result = iso_alpha2_to_dos_code(entry.iso_alpha2);
            assert_eq!(
                result,
                Some(entry.dos_code),
                "ISO code '{}' should map to DOS code {}",
                entry.iso_alpha2,
                entry.dos_code
            );
        }
    }

    #[test]
    fn test_extended_to_bytes_size_word_counts_the_bytes_after_it() {
        // The 41-byte record is the 1-byte info ID, the 2-byte size word,
        // then the 38 bytes that word counts (country code, code page and
        // the 34-byte AH=38h body). Asserting `len() == 41` checked only the
        // return type (MDD-BUG-CRU-01414); the size word is what a program
        // walking AH=65h records trusts.
        let info = DosCountryInfo::us();
        let bytes = info.extended_to_bytes();
        let size = u16::from_le_bytes([bytes[1], bytes[2]]);
        assert_eq!(usize::from(size), bytes.len() - 3);
    }

    #[test]
    fn test_country_table_covers_major_regions() {
        // Verify we have entries for all major regions
        assert!(country_from_code(1).is_some()); // US
        assert!(country_from_code(44).is_some()); // UK
        assert!(country_from_code(49).is_some()); // Germany
        assert!(country_from_code(33).is_some()); // France
        assert!(country_from_code(81).is_some()); // Japan
        assert!(country_from_code(86).is_some()); // China
        assert!(country_from_code(55).is_some()); // Brazil
    }

    // ========== Coverage: country_from_code for remaining regions ==========

    #[test]
    fn test_country_from_code_eastern_europe() {
        let ru = country_from_code(7).unwrap();
        assert_eq!(ru.date_format, 1); // DMY
        assert_eq!(ru.code_page, 866);
        assert_eq!(ru.time_format, 1);
        assert_eq!(ru.data_list_separator, b';');

        let hu = country_from_code(36).unwrap();
        assert_eq!(hu.date_format, 2); // YMD
        assert_eq!(hu.code_page, 852);

        let pl = country_from_code(48).unwrap();
        assert_eq!(pl.code_page, 852);
        assert_eq!(pl.decimal_separator, b',');
    }

    #[test]
    fn test_country_from_code_south_america() {
        let br = country_from_code(55).unwrap();
        assert_eq!(br.date_format, 1);
        assert_eq!(br.currency_symbol[0], b'R');
        assert_eq!(br.currency_symbol[1], b'$');
        assert_eq!(br.code_page, 850);
        assert_eq!(br.data_list_separator, b';');

        let ar = country_from_code(54).unwrap();
        assert_eq!(ar.currency_symbol[0], b'$');
        assert_eq!(ar.thousands_separator, b'.');
        assert_eq!(ar.decimal_separator, b',');
    }

    #[test]
    fn test_country_from_code_asia_pacific() {
        let kr = country_from_code(82).unwrap();
        assert_eq!(kr.date_format, 2); // YMD
        assert_eq!(kr.code_page, 949);

        let cn = country_from_code(86).unwrap();
        assert_eq!(cn.date_format, 2);
        assert_eq!(cn.code_page, 936);

        let tw = country_from_code(886).unwrap();
        assert_eq!(tw.date_format, 2);
        assert_eq!(tw.code_page, 950);
        assert_eq!(tw.currency_symbol[0], b'N');
        assert_eq!(tw.currency_symbol[1], b'T');
        assert_eq!(tw.currency_symbol[2], b'$');
    }

    #[test]
    fn test_country_from_code_nordic() {
        let dk = country_from_code(45).unwrap();
        assert_eq!(dk.code_page, 865);
        assert_eq!(dk.time_separator, b'.');

        let se = country_from_code(46).unwrap();
        assert_eq!(se.date_format, 2); // YMD
        assert_eq!(se.time_separator, b'.');

        let no = country_from_code(47).unwrap();
        assert_eq!(no.code_page, 865);

        let fi = country_from_code(358).unwrap();
        assert_eq!(fi.code_page, 850);
        assert_eq!(fi.time_separator, b'.');
    }

    #[test]
    fn test_country_from_code_latin_america_generic() {
        let la = country_from_code(3).unwrap();
        assert_eq!(la.date_format, 1);
        assert_eq!(la.currency_symbol[0], b'$');
        assert_eq!(la.code_page, 850);

        let mx = country_from_code(52).unwrap();
        assert_eq!(mx.date_format, 1);
        assert_eq!(mx.time_format, 1);
    }

    #[test]
    fn test_country_from_code_mediterranean() {
        let gr = country_from_code(30).unwrap();
        assert_eq!(gr.code_page, 737);

        let tr = country_from_code(90).unwrap();
        assert_eq!(tr.code_page, 857);

        let il = country_from_code(972).unwrap();
        assert_eq!(il.code_page, 862);
    }

    // ========== Coverage: to_country_info currency truncation ==========

    #[test]
    fn test_to_country_info_currency_padding() {
        // Verify that short currency symbols are zero-padded to 5 bytes
        let us = country_from_code(1).unwrap();
        assert_eq!(us.currency_symbol, [b'$', 0, 0, 0, 0]);

        // Multi-byte currency
        let ch = country_from_code(41).unwrap();
        assert_eq!(ch.currency_symbol[0], b'F');
        assert_eq!(ch.currency_symbol[1], b'r');
        assert_eq!(ch.currency_symbol[2], b'.');
        assert_eq!(ch.currency_symbol[3], 0);
        assert_eq!(ch.currency_symbol[4], 0);
    }

    // ========== Coverage: extended_to_bytes detailed field checks ==========

    #[test]
    fn test_extended_to_bytes_field_values() {
        let de = country_from_code(49).unwrap();
        let bytes = de.extended_to_bytes();

        // Header
        assert_eq!(bytes[0], 0x01); // Info ID
        assert_eq!(u16::from_le_bytes([bytes[1], bytes[2]]), 38); // Size
        assert_eq!(u16::from_le_bytes([bytes[3], bytes[4]]), 49); // Country code
        assert_eq!(u16::from_le_bytes([bytes[5], bytes[6]]), 850); // Code page

        // Body: date format at offset 7
        assert_eq!(u16::from_le_bytes([bytes[7], bytes[8]]), 1); // DMY

        // Currency at offset 9
        assert_eq!(bytes[9], b'E');

        // Thousands sep at offset 14 (7+7)
        assert_eq!(bytes[14], b'.');

        // Decimal sep at offset 16 (7+9)
        assert_eq!(bytes[16], b',');

        // Time format at offset 24 (7+17)
        assert_eq!(bytes[24], 1); // 24-hour
    }

    // ========== Coverage: write_to_buffer ASCIIZ terminators ==========

    #[test]
    fn test_write_to_buffer_asciiz_terminators() {
        let info = DosCountryInfo::us();
        let mut memory = Memory::new();
        let addr = 0x3000;
        info.write_to_buffer(&mut memory, addr);

        // Each separator is followed by a null byte (ASCIIZ)
        assert_eq!(memory.read_u8(addr + 0x08), 0); // thousands sep terminator
        assert_eq!(memory.read_u8(addr + 0x0A), 0); // decimal sep terminator
        assert_eq!(memory.read_u8(addr + 0x0C), 0); // date sep terminator
        assert_eq!(memory.read_u8(addr + 0x0E), 0); // time sep terminator
        assert_eq!(memory.read_u8(addr + 0x17), 0); // list sep terminator
    }

    // ========== Coverage: extended buffer consistency across countries ==========

    #[test]
    fn test_extended_to_bytes_matches_memory_write_all_regions() {
        // Verify extended_to_bytes and write_extended_to_buffer produce
        // identical output for several countries beyond just US
        for code in [1, 7, 33, 44, 49, 55, 81, 86] {
            let info = country_from_code(code).unwrap();
            let mut memory = Memory::new();
            let addr = 0x5000;
            info.write_extended_to_buffer(&mut memory, addr);

            let bytes = info.extended_to_bytes();
            for i in 0..41 {
                assert_eq!(
                    bytes[i],
                    memory.read_u8(addr + i),
                    "mismatch at byte {i} for country code {code}"
                );
            }
        }
    }

    // ========== Coverage: iso_alpha2_to_dos_code edge cases ==========

    #[test]
    fn test_iso_alpha2_to_dos_code_empty_and_partial() {
        assert_eq!(iso_alpha2_to_dos_code(""), None);
        assert_eq!(iso_alpha2_to_dos_code("X"), None);
        assert_eq!(iso_alpha2_to_dos_code("USA"), None); // 3 chars - no match
    }
}
