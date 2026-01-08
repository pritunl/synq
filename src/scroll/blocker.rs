use std::fs::{File, OpenOptions};
use std::io::{Read, Write};
use std::mem;
use std::os::unix::io::AsRawFd;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use tokio_util::sync::CancellationToken;
use tracing::trace;

use crate::errors::{Error, ErrorKind, Result};
use crate::utils;
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
    last_scroll: Arc<AtomicU64>,
}

impl ScrollBlocker {
    pub fn new(device_path: impl AsRef<Path>, last_scroll: Arc<AtomicU64>) -> Result<Self> {
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
            last_scroll
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

        let is_scroll = event.type_ == EV_REL as u16
            && (event.code == REL_WHEEL
                || event.code == REL_HWHEEL
                || event.code == REL_WHEEL_HI_RES
                || event.code == REL_HWHEEL_HI_RES);

        if is_scroll {
            self.last_scroll.store(utils::mono_time_ms(), Ordering::SeqCst);
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
