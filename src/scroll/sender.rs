use std::fs::File;
use std::io::Write;
use std::mem;
use std::ptr;

use tracing::trace;

use crate::errors::{Error, ErrorKind, Result};
use super::event::InputEvent;
use super::utils::setup_uinput_scroll;
use super::constants::{
    EV_SYN,
    EV_REL,
    REL_WHEEL,
    REL_HWHEEL,
    REL_WHEEL_HI_RES,
    REL_HWHEEL_HI_RES,
    SYN_REPORT,
};

pub struct ScrollSender {
    uinput: File,
}

impl ScrollSender {
    pub fn new() -> Result<Self> {
        let uinput = setup_uinput_scroll()?;
        Ok(Self {
            uinput
        })
    }

    pub fn send(&mut self, delta_x: f64, delta_y: f64) -> Result<()> {
        let now = unsafe {
            let mut tv: libc::timeval = mem::zeroed();
            libc::gettimeofday(&mut tv, ptr::null_mut());
            tv
        };

        // Convert libinput scroll values to v120 units
        // libinput uses 15 degrees per scroll click by default
        // v120 uses 120 units per scroll click
        // So multiply by 120/15 = 8
        const SCALE: f64 = 8.0;

        let hi_res_y = (delta_y * SCALE) as i32;
        let hi_res_x = (delta_x * SCALE) as i32;

        trace!(
            delta_x = delta_x,
            delta_y = delta_y,
            hi_res_x = hi_res_x,
            hi_res_y = hi_res_y,
            "Sending scroll event"
        );

        if hi_res_y != 0 {
            let event = InputEvent {
                tv_sec: now.tv_sec,
                tv_usec: now.tv_usec,
                type_: EV_REL as u16,
                code: REL_WHEEL_HI_RES,
                value: hi_res_y,
            };
            self.write_event(&event)?;
        }

        if hi_res_x != 0 {
            let event = InputEvent {
                tv_sec: now.tv_sec,
                tv_usec: now.tv_usec,
                type_: EV_REL as u16,
                code: REL_HWHEEL_HI_RES,
                value: hi_res_x,
            };
            self.write_event(&event)?;
        }

        // Calculate discrete scroll clicks (120 units = 1 click)
        let discrete_y = hi_res_y / 120;
        let discrete_x = hi_res_x / 120;

        if discrete_y != 0 {
            let event = InputEvent {
                tv_sec: now.tv_sec,
                tv_usec: now.tv_usec,
                type_: EV_REL as u16,
                code: REL_WHEEL,
                value: discrete_y,
            };
            self.write_event(&event)?;
        }

        if discrete_x != 0 {
            let event = InputEvent {
                tv_sec: now.tv_sec,
                tv_usec: now.tv_usec,
                type_: EV_REL as u16,
                code: REL_HWHEEL,
                value: discrete_x,
            };
            self.write_event(&event)?;
        }

        let syn_event = InputEvent {
            tv_sec: now.tv_sec,
            tv_usec: now.tv_usec,
            type_: EV_SYN,
            code: SYN_REPORT,
            value: 0,
        };
        self.write_event(&syn_event)?;

        Ok(())
    }

    fn write_event(&mut self, event: &InputEvent) -> Result<()> {
        let bytes: [u8; mem::size_of::<InputEvent>()] = unsafe { mem::transmute(*event) };
        (&self.uinput).write_all(&bytes).map_err(|e| {
            Error::wrap(e, ErrorKind::Write)
                .with_msg("scroll: Failed to write scroll event")
        })
    }
}
