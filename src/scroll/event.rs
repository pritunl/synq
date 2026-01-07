use super::constants::{ScrollSource};

pub struct ScrollEvent {
    pub source: ScrollSource,
    pub delta_x: f64,
    pub delta_y: f64,
}

#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct InputEvent {
    pub tv_sec: libc::time_t,
    pub tv_usec: libc::suseconds_t,
    pub type_: u16,
    pub code: u16,
    pub value: i32,
}
