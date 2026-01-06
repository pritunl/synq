pub(crate) const EVIOCGRAB: libc::c_ulong = 0x40044590;
pub(crate) const UI_SET_EVBIT: libc::c_ulong = 0x40045564;
pub(crate) const UI_SET_KEYBIT: libc::c_ulong = 0x40045565;
pub(crate) const UI_SET_RELBIT: libc::c_ulong = 0x40045566;
pub(crate) const UI_SET_ABSBIT: libc::c_ulong = 0x40045567;
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScrollSource {
    Wheel,
    Finger,
    Continuous,
}
