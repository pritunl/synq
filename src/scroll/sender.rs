use std::mem;
use std::ptr;

use crate::errors::trace;

use crate::errors::Result;
use super::event::InputEvent;
use super::utils::SharedUinput;
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
    uinput: SharedUinput,
}

impl ScrollSender {
    pub fn new(uinput: SharedUinput) -> Self {
        Self { uinput }
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

        // Calculate discrete scroll clicks (120 units = 1 click)
        let discrete_y = hi_res_y / 120;
        let discrete_x = hi_res_x / 120;

        let mut events = Vec::with_capacity(5);

        if hi_res_y != 0 {
            events.push(InputEvent {
                tv_sec: now.tv_sec,
                tv_usec: now.tv_usec,
                type_: EV_REL as u16,
                code: REL_WHEEL_HI_RES,
                value: hi_res_y,
            });
        }

        if hi_res_x != 0 {
            events.push(InputEvent {
                tv_sec: now.tv_sec,
                tv_usec: now.tv_usec,
                type_: EV_REL as u16,
                code: REL_HWHEEL_HI_RES,
                value: hi_res_x,
            });
        }

        if discrete_y != 0 {
            events.push(InputEvent {
                tv_sec: now.tv_sec,
                tv_usec: now.tv_usec,
                type_: EV_REL as u16,
                code: REL_WHEEL,
                value: discrete_y,
            });
        }

        if discrete_x != 0 {
            events.push(InputEvent {
                tv_sec: now.tv_sec,
                tv_usec: now.tv_usec,
                type_: EV_REL as u16,
                code: REL_HWHEEL,
                value: discrete_x,
            });
        }

        events.push(InputEvent {
            tv_sec: now.tv_sec,
            tv_usec: now.tv_usec,
            type_: EV_SYN,
            code: SYN_REPORT,
            value: 0,
        });

        self.uinput.write_events(&events)
    }
}
