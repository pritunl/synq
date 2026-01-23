use std::fs::{File, OpenOptions};
use std::io::Read;
use std::mem;
use std::os::unix::io::AsRawFd;
use std::path::Path;

use tokio_util::sync::CancellationToken;
use crate::errors::trace;
use crate::utils::mono_time_ms;

use crate::errors::{Error, ErrorKind, Result};
use crate::transport::ActiveState;
use super::constants::{
    EVIOCGRAB,
    EV_REL,
    REL_WHEEL,
    REL_HWHEEL,
    REL_WHEEL_HI_RES,
    REL_HWHEEL_HI_RES,
};
use super::event::InputEvent;
use super::utils::SharedUinput;

pub struct ScrollBlocker {
    device: File,
    uinput: SharedUinput,
    active_state: ActiveState,
    on_scroll: Option<Box<dyn Fn() + Send>>,
}

impl ScrollBlocker {
    pub fn new(
        device_path: impl AsRef<Path>,
        uinput: SharedUinput,
        active_state: ActiveState,
        on_scroll: Option<Box<dyn Fn() + Send>>,
    ) -> Result<Self> {
        let path = device_path.as_ref();

        let device = OpenOptions::new()
            .read(true)
            .open(path)
            .map_err(|e| {
                Error::wrap(e, ErrorKind::Read)
                    .with_msg("scroll: Failed to open input device")
                    .with_ctx("path", path.display())
            })?;

        let fd = device.as_raw_fd();

        if unsafe { libc::ioctl(fd, EVIOCGRAB, 1) } != 0 {
            return Err(Error::wrap(std::io::Error::last_os_error(), ErrorKind::Exec)
                .with_msg("scroll: Failed to grab input device")
                .with_ctx("path", path.display()));
        }

        Ok(Self {
            device,
            uinput,
            active_state,
            on_scroll,
        })
    }

    pub fn process_events(&mut self) -> Result<()> {
        let mut buf = [0u8; mem::size_of::<InputEvent>()];

        self.device.read_exact(&mut buf).map_err(|e| {
            Error::wrap(e, ErrorKind::Read)
                .with_msg("scroll: Failed to read input event")
        })?;

        // Read type and code directly from buffer without full transmute
        // InputEvent layout: tv_sec (8), tv_usec (8), type (2), code (2), value (4)
        if u16::from_ne_bytes([buf[16], buf[17]]) == EV_REL as u16 {
            let code = u16::from_ne_bytes([buf[18], buf[19]]);
            if code == REL_WHEEL
                || code == REL_HWHEEL
                || code == REL_WHEEL_HI_RES
                || code == REL_HWHEEL_HI_RES
            {
                self.active_state.set_last_scroll(mono_time_ms());
                trace!(code = code, "Blocked scroll event");
            } else {
                self.uinput.write_raw(&buf)?;
            }
        } else {
            self.uinput.write_raw(&buf)?;
        }

        if !self.active_state.is_host_active() {
            trace!("Not active, sending active request");
            if let Some(ref on_scroll) = self.on_scroll {
                on_scroll();
            }
        }

        Ok(())
    }

    pub fn run(&mut self, cancel: CancellationToken) -> Result<()> {
        while !cancel.is_cancelled() {
            self.process_events()?;
        }
        Ok(())
    }

    pub fn device_fd(&self) -> i32 {
        self.device.as_raw_fd()
    }

    pub fn release(fd: i32) {
        unsafe { libc::ioctl(fd, EVIOCGRAB, 0) };
    }
}

impl Drop for ScrollBlocker {
    fn drop(&mut self) {
        let fd = self.device.as_raw_fd();
        unsafe { libc::ioctl(fd, EVIOCGRAB, 0) };
    }
}
