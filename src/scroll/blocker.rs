use std::fs::{File, OpenOptions};
use std::io::{Read, Write};
use std::mem;
use std::os::unix::io::AsRawFd;
use std::path::Path;

use tokio_util::sync::CancellationToken;
use crate::errors::trace;
use crate::errors::info;

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
use super::utils::setup_uinput_clone;

pub struct ScrollBlocker {
    device: File,
    uinput: File,
    active_state: ActiveState,
    host_public_key: String,
    on_scroll: Option<Box<dyn Fn() + Send>>,
}

impl ScrollBlocker {
    pub fn new(
        device_path: impl AsRef<Path>,
        active_state: ActiveState,
        host_public_key: String,
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

        let uinput = setup_uinput_clone(fd)?;

        if unsafe { libc::ioctl(fd, EVIOCGRAB, 1) } != 0 {
            return Err(Error::wrap(std::io::Error::last_os_error(), ErrorKind::Exec)
                .with_msg("scroll: Failed to grab input device")
                .with_ctx("path", path.display()));
        }

        Ok(Self {
            device,
            uinput,
            active_state,
            host_public_key,
            on_scroll,
        })
    }

    pub fn process_events(&mut self) -> Result<()> {
        let mut buf = [0u8; mem::size_of::<InputEvent>()];

        self.device.read_exact(&mut buf).map_err(|e| {
            Error::wrap(e, ErrorKind::Read)
                .with_msg("scroll: Failed to read input event")
        })?;

        let event: InputEvent = unsafe {
            mem::transmute(buf)
        };

        let inactive = self.active_state.get_active_peer()
            .map(|p| p != self.host_public_key)
            .unwrap_or(true);

        let is_scroll = event.type_ == EV_REL as u16
            && (event.code == REL_WHEEL
                || event.code == REL_HWHEEL
                || event.code == REL_WHEEL_HI_RES
                || event.code == REL_HWHEEL_HI_RES);

        if is_scroll {
            trace!(code = event.code, value = event.value, "Blocked scroll event");
        } else {
            let bytes: [u8; mem::size_of::<InputEvent>()] = unsafe {
                mem::transmute(event)
            };

            (&self.uinput).write_all(&bytes).map_err(|e| {
                Error::wrap(e, ErrorKind::Write)
                    .with_msg("scroll: Failed to write event to uinput")
            })?;
        }

        if inactive {
            info!("Not active, sending active request");
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
