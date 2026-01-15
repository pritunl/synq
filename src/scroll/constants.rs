pub(crate) const EV_MAX: usize = 0x1f;
pub(crate) const KEY_MAX: usize = 0x2ff;
pub(crate) const REL_MAX: usize = 0x0f;
pub(crate) const ABS_MAX: usize = 0x3f;
pub(crate) const MSC_MAX: usize = 0x07;
pub(crate) const SW_MAX: usize = 0x10;
pub(crate) const LED_MAX: usize = 0x0f;
pub(crate) const SND_MAX: usize = 0x07;
pub(crate) const FF_MAX: usize = 0x7f;

pub(crate) const EV_MSC: libc::c_int = 0x04;
pub(crate) const EV_SW: libc::c_int = 0x05;
pub(crate) const EV_LED: libc::c_int = 0x11;
pub(crate) const EV_SND: libc::c_int = 0x12;
pub(crate) const EV_FF: libc::c_int = 0x15;

pub(crate) const EVIOCGRAB: libc::c_ulong = 0x40044590;
pub(crate) const EVIOCGID: libc::c_ulong = (2u64 << 30) | (8u64 << 16) | (0x45u64 << 8) | 0x02;

pub(crate) const UI_SET_EVBIT: libc::c_ulong = 0x40045564;
pub(crate) const UI_SET_KEYBIT: libc::c_ulong = 0x40045565;
pub(crate) const UI_SET_RELBIT: libc::c_ulong = 0x40045566;
pub(crate) const UI_SET_ABSBIT: libc::c_ulong = 0x40045567;
pub(crate) const UI_SET_MSCBIT: libc::c_ulong = 0x40045568;
pub(crate) const UI_SET_LEDBIT: libc::c_ulong = 0x40045569;
pub(crate) const UI_SET_SNDBIT: libc::c_ulong = 0x4004556a;
pub(crate) const UI_SET_FFBIT: libc::c_ulong = 0x4004556b;
pub(crate) const UI_SET_SWBIT: libc::c_ulong = 0x4004556d;
pub(crate) const UI_ABS_SETUP: libc::c_ulong = 0x401c5504;
pub(crate) const UI_DEV_SETUP: libc::c_ulong = 0x405c5503;
pub(crate) const UI_DEV_CREATE: libc::c_ulong = 0x5501;

pub(crate) const EV_SYN: u16 = 0x00;
pub(crate) const EV_KEY: libc::c_int = 0x01;
pub(crate) const EV_REL: libc::c_int = 0x02;
pub(crate) const EV_ABS: libc::c_int = 0x03;

pub(crate) const REL_HWHEEL: u16 = 0x06;
pub(crate) const REL_WHEEL: u16 = 0x08;
pub(crate) const REL_WHEEL_HI_RES: u16 = 0x0b;
pub(crate) const REL_HWHEEL_HI_RES: u16 = 0x0c;

pub(crate) const SYN_REPORT: u16 = 0x00;

pub(crate) const ABS_MT_SLOT: u16 = 0x2f;
pub(crate) const ABS_MT_TRACKING_ID: u16 = 0x39;
pub(crate) const ABS_MT_POSITION_X: u16 = 0x35;
pub(crate) const ABS_MT_POSITION_Y: u16 = 0x36;

pub(crate) const BTN_TOUCH: u16 = 0x14a;
pub(crate) const BTN_TOOL_FINGER: u16 = 0x145;
pub(crate) const BTN_TOOL_DOUBLETAP: u16 = 0x14d;

pub(crate) const SCROLL_DEVICE_NAME: &[u8] = b"Virtual Scroll Device";
pub(crate) const SCROLL_DEVICE_ID: [u16; 4] = [0x06, 0x628, 0x1, 0x1];

pub(crate) const TOUCHPAD_DEVICE_NAME: &[u8] = b"Virtual Touchpad";
pub(crate) const TOUCHPAD_DEVICE_ID: [u16; 4] = [0x03, 0x0001, 0x0001, 0x0001];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScrollSource {
    Wheel,
    Finger,
    Continuous,
}
