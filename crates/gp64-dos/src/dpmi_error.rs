//! DPMI error codes.

pub const DPMI_ERR_UNSUPPORTED: u16 = 0x8001;
pub const DPMI_ERR_RESOURCE_UNAVAILABLE: u16 = 0x8010;
pub const DPMI_ERR_DESC_UNAVAILABLE: u16 = 0x8011;
pub const DPMI_ERR_INVALID_VALUE: u16 = 0x8021;
pub const DPMI_ERR_INVALID_SELECTOR: u16 = 0x8022;
pub const DPMI_ERR_INVALID_HANDLE: u16 = 0x8023;
pub const DPMI_ERR_INVALID_CALLBACK: u16 = 0x8024;
pub const DPMI_ERR_INVALID_LINEAR: u16 = 0x8025;
pub const DPMI_ERR_LINEAR_UNAVAILABLE: u16 = 0x8012;
pub const DPMI_ERR_PHYS_UNAVAILABLE: u16 = 0x8013;
pub const DPMI_ERR_BACKING_STORE_UNAVAILABLE: u16 = 0x8014;
pub const DPMI_ERR_CALLBACK_UNAVAIL: u16 = 0x8015;
pub const DPMI_ERR_HANDLE_UNAVAILABLE: u16 = 0x8016;

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::*;

    #[test]
    fn test_dpmi_error_constants() {
        assert_eq!(DPMI_ERR_UNSUPPORTED, 0x8001);
        assert_eq!(DPMI_ERR_RESOURCE_UNAVAILABLE, 0x8010);
        assert_eq!(DPMI_ERR_DESC_UNAVAILABLE, 0x8011);
        assert_eq!(DPMI_ERR_LINEAR_UNAVAILABLE, 0x8012);
        assert_eq!(DPMI_ERR_PHYS_UNAVAILABLE, 0x8013);
        assert_eq!(DPMI_ERR_BACKING_STORE_UNAVAILABLE, 0x8014);
        assert_eq!(DPMI_ERR_CALLBACK_UNAVAIL, 0x8015);
        assert_eq!(DPMI_ERR_HANDLE_UNAVAILABLE, 0x8016);
        assert_eq!(DPMI_ERR_INVALID_VALUE, 0x8021);
        assert_eq!(DPMI_ERR_INVALID_SELECTOR, 0x8022);
        assert_eq!(DPMI_ERR_INVALID_HANDLE, 0x8023);
        assert_eq!(DPMI_ERR_INVALID_CALLBACK, 0x8024);
        assert_eq!(DPMI_ERR_INVALID_LINEAR, 0x8025);
    }
}
