//! Analog gameport with no attached joystick.
/// An unconnected gameport has no capacitor discharge or pressed buttons.
#[derive(Clone, Debug, Default)]
pub struct Joystick;
impl Joystick {
    pub fn new() -> Self {
        Self
    }
    pub fn read(&self) -> u8 {
        0xff
    }
    pub fn write(&mut self, _value: u8) {}
}
