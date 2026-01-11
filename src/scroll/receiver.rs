use std::fs::{File, OpenOptions};
use std::os::unix::fs::OpenOptionsExt;
use std::os::unix::io::{AsRawFd, OwnedFd};
use std::path::Path;

use input::event::pointer::{Axis, PointerEvent, PointerScrollEvent};
use input::{Event, Libinput, LibinputInterface};
use libc::{O_RDONLY, O_RDWR, O_WRONLY, O_ACCMODE};
use crate::errors::trace;

use crate::errors::{Error, ErrorKind, Result};
use super::event::ScrollEvent;
use super::constants::{
    ScrollSource,
};

struct Interface;

impl LibinputInterface for Interface {
    fn open_restricted(
        &mut self,
        path: &Path,
        flags: i32,
    ) -> std::result::Result<OwnedFd, i32> {
        let access_mode = flags & O_ACCMODE;

        OpenOptions::new()
            .custom_flags(flags)
            .read(access_mode == O_RDONLY || access_mode == O_RDWR)
            .write(access_mode == O_WRONLY || access_mode == O_RDWR)
            .open(path)
            .map(|file| file.into())
            .map_err(|err| err.raw_os_error().unwrap_or(-1))
    }

    fn close_restricted(&mut self, fd: OwnedFd) {
        drop(File::from(fd));
    }
}

pub struct ScrollReceiver {
    libinput: Libinput,
}

impl ScrollReceiver {
    pub fn new(device_path: impl AsRef<Path>) -> Result<Self> {
        let path = device_path.as_ref();

        let mut libinput = Libinput::new_from_path(Interface);
        libinput.path_add_device(path.to_str().ok_or_else(|| {
            Error::new(ErrorKind::Parse).with_msg("scroll: Invalid device path")
        })?).ok_or_else(|| {
            Error::new(ErrorKind::Read)
                .with_msg("scroll: Failed to add device to libinput")
                .with_ctx("path", path.display())
        })?;

        Ok(Self {
            libinput
        })
    }

    pub fn read_event(&mut self) -> Result<Option<ScrollEvent>> {
        let mut pfd = libc::pollfd {
            fd: self.libinput.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        };

        let ret = unsafe {
            libc::poll(&mut pfd, 1, -1)
        };
        if ret < 0 {
            return Err(Error::wrap(std::io::Error::last_os_error(), ErrorKind::Read)
                .with_msg("scroll: Poll failed"));
        }

        self.libinput.dispatch().map_err(|e| {
            Error::wrap(e, ErrorKind::Read)
                .with_msg("scroll: Failed to dispatch libinput events")
        })?;

        for event in &mut self.libinput {
            if let Event::Pointer(pointer_event) = event {
                match pointer_event {
                    PointerEvent::ScrollWheel(wheel_event) => {
                        let delta_x = if wheel_event.has_axis(Axis::Horizontal) {
                            wheel_event.scroll_value(Axis::Horizontal)
                        } else {
                            0.0
                        };
                        let delta_y = if wheel_event.has_axis(Axis::Vertical) {
                            wheel_event.scroll_value(Axis::Vertical)
                        } else {
                            0.0
                        };

                        trace!(
                            source = ?ScrollSource::Wheel,
                            delta_x = delta_x,
                            delta_y = delta_y,
                            "Scroll event"
                        );

                        return Ok(Some(ScrollEvent {
                            source: ScrollSource::Wheel,
                            delta_x,
                            delta_y,
                        }));
                    }
                    PointerEvent::ScrollFinger(finger_event) => {
                        let delta_x = if finger_event.has_axis(Axis::Horizontal) {
                            finger_event.scroll_value(Axis::Horizontal)
                        } else {
                            0.0
                        };
                        let delta_y = if finger_event.has_axis(Axis::Vertical) {
                            finger_event.scroll_value(Axis::Vertical)
                        } else {
                            0.0
                        };

                        trace!(
                            source = ?ScrollSource::Finger,
                            delta_x = delta_x,
                            delta_y = delta_y,
                            "Scroll event",
                        );

                        return Ok(Some(ScrollEvent {
                            source: ScrollSource::Finger,
                            delta_x,
                            delta_y,
                        }));
                    }
                    PointerEvent::ScrollContinuous(continuous_event) => {
                        let delta_x = if continuous_event.has_axis(Axis::Horizontal) {
                            continuous_event.scroll_value(Axis::Horizontal)
                        } else {
                            0.0
                        };
                        let delta_y = if continuous_event.has_axis(Axis::Vertical) {
                            continuous_event.scroll_value(Axis::Vertical)
                        } else {
                            0.0
                        };

                        trace!(
                            source = ?ScrollSource::Continuous,
                            delta_x = delta_x,
                            delta_y = delta_y,
                            "Scroll event"
                        );

                        return Ok(Some(ScrollEvent {
                            source: ScrollSource::Continuous,
                            delta_x,
                            delta_y,
                        }));
                    }
                    _ => {}
                }
            }
        }

        Ok(None)
    }
}
