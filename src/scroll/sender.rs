use std::mem;
use std::ptr;

use crate::errors::{Result, trace};
use crate::transport::{Transport};
use crate::utils;

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
    SCROLL_TTL,
    BLUR_TTL,
};

const EVENT_SIZE: usize = mem::size_of::<InputEvent>();

pub struct ScrollSender {
    uinput: SharedUinput,
    transport: Transport,
}

impl ScrollSender {
    pub fn new(uinput: SharedUinput, transport: Transport) -> Self {
        Self {
            uinput,
            transport,
        }
    }

    pub fn send(&mut self, delta_x: f64, delta_y: f64) -> Result<()> {
        let input_now = unsafe {
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

        let mut buf = [0u8; 5 * EVENT_SIZE];
        let mut offset = 0;

        if hi_res_y != 0 {
            let event = InputEvent {
                tv_sec: input_now.tv_sec,
                tv_usec: input_now.tv_usec,
                type_: EV_REL as u16,
                code: REL_WHEEL_HI_RES,
                value: hi_res_y,
            };
            let bytes: [u8; EVENT_SIZE] = unsafe { mem::transmute(event) };
            buf[offset..offset + EVENT_SIZE].copy_from_slice(&bytes);
            offset += EVENT_SIZE;
        }

        if hi_res_x != 0 {
            let event = InputEvent {
                tv_sec: input_now.tv_sec,
                tv_usec: input_now.tv_usec,
                type_: EV_REL as u16,
                code: REL_HWHEEL_HI_RES,
                value: hi_res_x,
            };
            let bytes: [u8; EVENT_SIZE] = unsafe { mem::transmute(event) };
            buf[offset..offset + EVENT_SIZE].copy_from_slice(&bytes);
            offset += EVENT_SIZE;
        }

        if discrete_y != 0 {
            let event = InputEvent {
                tv_sec: input_now.tv_sec,
                tv_usec: input_now.tv_usec,
                type_: EV_REL as u16,
                code: REL_WHEEL,
                value: discrete_y,
            };
            let bytes: [u8; EVENT_SIZE] = unsafe { mem::transmute(event) };
            buf[offset..offset + EVENT_SIZE].copy_from_slice(&bytes);
            offset += EVENT_SIZE;
        }

        if discrete_x != 0 {
            let event = InputEvent {
                tv_sec: input_now.tv_sec,
                tv_usec: input_now.tv_usec,
                type_: EV_REL as u16,
                code: REL_HWHEEL,
                value: discrete_x,
            };
            let bytes: [u8; EVENT_SIZE] = unsafe { mem::transmute(event) };
            buf[offset..offset + EVENT_SIZE].copy_from_slice(&bytes);
            offset += EVENT_SIZE;
        }

        let syn_event = InputEvent {
            tv_sec: input_now.tv_sec,
            tv_usec: input_now.tv_usec,
            type_: EV_SYN,
            code: SYN_REPORT,
            value: 0,
        };
        let bytes: [u8; EVENT_SIZE] = unsafe { mem::transmute(syn_event) };
        buf[offset..offset + EVENT_SIZE].copy_from_slice(&bytes);
        offset += EVENT_SIZE;

        let now = utils::mono_time_ms();
        let last_scroll = self.transport.active_state.get_last_scroll();

        if last_scroll > 0 && now - last_scroll > SCROLL_TTL {
            let last_blur = self.transport.active_state.get_last_blur();
            if last_blur == 0 {
                self.transport.active_state.set_last_blur(now);
            } else if now - last_blur > BLUR_TTL {
                if self.transport.active_state.is_host_active() {
                    self.transport.send_deactivate_request();
                }
                return Ok(());
            }
        } else {
            self.transport.active_state.set_last_blur(0);
        }

        self.uinput.write_raw(&buf[..offset])
    }
}
