use std::fs::{File, OpenOptions};
use std::io::Write;
use std::mem;
use std::os::unix::fs::OpenOptionsExt;
use std::os::unix::io::{AsRawFd, OwnedFd};
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use input::event::pointer::{Axis, PointerEvent, PointerScrollEvent};
use input::{Event, Libinput, LibinputInterface};
use libc::{O_RDONLY, O_RDWR, O_WRONLY};
use tracing::{info, trace};

use crate::errors::{Error, ErrorKind, Result};
use crate::utils;

const EVIOCGRAB: libc::c_ulong = 0x40044590;
const UI_SET_EVBIT: libc::c_ulong = 0x40045564;
const UI_SET_KEYBIT: libc::c_ulong = 0x40045565;
const UI_SET_RELBIT: libc::c_ulong = 0x40045566;
const UI_SET_ABSBIT: libc::c_ulong = 0x40045567;
const UI_ABS_SETUP: libc::c_ulong = 0x401c5504;
const UI_DEV_SETUP: libc::c_ulong = 0x405c5503;
const UI_DEV_CREATE: libc::c_ulong = 0x5501;

const EV_SYN: u16 = 0x00;
const EV_KEY: libc::c_int = 0x01;
const EV_REL: libc::c_int = 0x02;
const EV_ABS: libc::c_int = 0x03;

const REL_HWHEEL: u16 = 0x06;
const REL_WHEEL: u16 = 0x08;
const REL_WHEEL_HI_RES: u16 = 0x0b;
const REL_HWHEEL_HI_RES: u16 = 0x0c;

const SYN_REPORT: u16 = 0x00;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScrollSource {
    Wheel,
    Finger,
    Continuous,
}

#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
struct InputEvent {
    tv_sec: libc::time_t,
    tv_usec: libc::suseconds_t,
    type_: u16,
    code: u16,
    value: i32,
}

#[repr(C)]
struct UinputAbsSetup {
    code: u16,
    _padding: u16,
    absinfo: AbsInfo,
}

#[repr(C)]
struct AbsInfo {
    value: i32,
    minimum: i32,
    maximum: i32,
    fuzz: i32,
    flat: i32,
    resolution: i32,
}

#[repr(C)]
struct UinputSetup {
    id: [u16; 4],
    name: [u8; 80],
    ff_effects_max: u32,
}

struct Interface;

impl LibinputInterface for Interface {
    fn open_restricted(&mut self, path: &Path, flags: i32) -> std::result::Result<OwnedFd, i32> {
        OpenOptions::new()
            .custom_flags(flags)
            .read((flags & O_RDONLY != 0) | (flags & O_RDWR != 0))
            .write((flags & O_WRONLY != 0) | (flags & O_RDWR != 0))
            .open(path)
            .map(|file| file.into())
            .map_err(|err| err.raw_os_error().unwrap_or(-1))
    }

    fn close_restricted(&mut self, fd: OwnedFd) {
        drop(File::from(fd));
    }
}

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
        if unsafe { libc::ioctl(fd, EVIOCGRAB, 1) } != 0 {
            return Err(Error::wrap(std::io::Error::last_os_error(), ErrorKind::Exec)
                .with_msg("scroll: Failed to grab input device")
                .with_ctx("path", path.display()));
        }

        let uinput = setup_uinput()?;

        Ok(Self { device, uinput, last_scroll })
    }

    pub fn process_events(&mut self) -> Result<()> {
        use std::io::Read;

        let mut buf = [0u8; mem::size_of::<InputEvent>()];

        self.device.read_exact(&mut buf).map_err(|e| {
            Error::wrap(e, ErrorKind::Read)
                .with_msg("scroll: Failed to read input event")
        })?;

        let event: InputEvent = unsafe { mem::transmute(buf) };

        let is_scroll = event.type_ == EV_REL as u16
            && (event.code == REL_WHEEL
                || event.code == REL_HWHEEL
                || event.code == REL_WHEEL_HI_RES
                || event.code == REL_HWHEEL_HI_RES);

        if is_scroll {
            self.last_scroll.store(utils::mono_time_ms(), Ordering::SeqCst);
            trace!(code = event.code, value = event.value, "Blocked scroll event");
        } else {
            let bytes: [u8; mem::size_of::<InputEvent>()] = unsafe { mem::transmute(event) };
            (&self.uinput).write_all(&bytes).map_err(|e| {
                Error::wrap(e, ErrorKind::Write)
                    .with_msg("scroll: Failed to write event to uinput")
            })?;
        }

        Ok(())
    }

    pub fn run_blocking(&mut self) -> Result<()> {
        loop {
            self.process_events()?;
        }
    }
}

pub struct ScrollEvent {
    pub source: ScrollSource,
    pub delta_x: f64,
    pub delta_y: f64,
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

        Ok(Self { libinput })
    }

    pub fn read_event(&mut self) -> Result<Option<ScrollEvent>> {
        let mut pfd = libc::pollfd {
            fd: self.libinput.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        };

        let ret = unsafe { libc::poll(&mut pfd, 1, -1) };
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

                        info!(
                            source = "ScrollWheel",
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

                        info!(
                            source = "ScrollFinger",
                            delta_x = delta_x,
                            delta_y = delta_y,
                            "Scroll event"
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

                        info!(
                            source = "ScrollContinuous",
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

pub struct ScrollSender {
    uinput: File,
}

impl ScrollSender {
    pub fn new() -> Result<Self> {
        let uinput = setup_scroll_uinput()?;
        Ok(Self { uinput })
    }

    pub fn send(&mut self, delta_x: f64, delta_y: f64) -> Result<()> {
        let now = unsafe {
            let mut tv: libc::timeval = mem::zeroed();
            libc::gettimeofday(&mut tv, std::ptr::null_mut());
            tv
        };

        // Convert libinput scroll values to v120 units
        // libinput uses 15 degrees per scroll click by default
        // v120 uses 120 units per scroll click
        // So multiply by 120/15 = 8
        const SCALE: f64 = 8.0;

        let hi_res_y = (delta_y * SCALE) as i32;
        let hi_res_x = (delta_x * SCALE) as i32;

        info!(
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

fn setup_uinput() -> Result<File> {
    let uinput = OpenOptions::new()
        .read(true)
        .write(true)
        .open("/dev/uinput")
        .map_err(|e| {
            Error::wrap(e, ErrorKind::Read)
                .with_msg("scroll: Failed to open uinput device")
        })?;

    let fd = uinput.as_raw_fd();

    unsafe {
        if libc::ioctl(fd, UI_SET_EVBIT, EV_KEY) < 0 {
            return Err(Error::wrap(std::io::Error::last_os_error(), ErrorKind::Exec)
                .with_msg("scroll: Failed to set EV_KEY"));
        }
        if libc::ioctl(fd, UI_SET_EVBIT, EV_REL) < 0 {
            return Err(Error::wrap(std::io::Error::last_os_error(), ErrorKind::Exec)
                .with_msg("scroll: Failed to set EV_REL"));
        }
        if libc::ioctl(fd, UI_SET_EVBIT, EV_ABS) < 0 {
            return Err(Error::wrap(std::io::Error::last_os_error(), ErrorKind::Exec)
                .with_msg("scroll: Failed to set EV_ABS"));
        }

        libc::ioctl(fd, UI_SET_RELBIT, REL_WHEEL as libc::c_int);
        libc::ioctl(fd, UI_SET_RELBIT, REL_HWHEEL as libc::c_int);

        for btn in [272, 273, 274, 275, 276, 330, 336, 337] {
            libc::ioctl(fd, UI_SET_KEYBIT, btn as libc::c_int);
        }

        libc::ioctl(fd, UI_SET_ABSBIT, 0x00 as libc::c_int); // ABS_X
        libc::ioctl(fd, UI_SET_ABSBIT, 0x01 as libc::c_int); // ABS_Y

        let abs_x = UinputAbsSetup {
            code: 0x00,
            _padding: 0,
            absinfo: AbsInfo {
                value: 0,
                minimum: 0,
                maximum: 32767,
                fuzz: 0,
                flat: 0,
                resolution: 0,
            },
        };
        libc::ioctl(fd, UI_ABS_SETUP, &abs_x);

        let abs_y = UinputAbsSetup {
            code: 0x01,
            _padding: 0,
            absinfo: AbsInfo {
                value: 0,
                minimum: 0,
                maximum: 32767,
                fuzz: 0,
                flat: 0,
                resolution: 0,
            },
        };
        libc::ioctl(fd, UI_ABS_SETUP, &abs_y);

        let mut setup: UinputSetup = mem::zeroed();
        setup.id = [0x06, 0x627, 0x3, 0x2];
        let name = b"Virtual Virtio Tablet";
        setup.name[..name.len()].copy_from_slice(name);

        libc::ioctl(fd, UI_DEV_SETUP, &setup);
        libc::ioctl(fd, UI_DEV_CREATE, 0);
    }

    std::thread::sleep(std::time::Duration::from_millis(200));
    Ok(uinput)
}

fn setup_scroll_uinput() -> Result<File> {
    let uinput = OpenOptions::new()
        .read(true)
        .write(true)
        .open("/dev/uinput")
        .map_err(|e| {
            Error::wrap(e, ErrorKind::Read)
                .with_msg("scroll: Failed to open uinput device")
        })?;

    let fd = uinput.as_raw_fd();

    unsafe {
        if libc::ioctl(fd, UI_SET_EVBIT, EV_REL) < 0 {
            return Err(Error::wrap(std::io::Error::last_os_error(), ErrorKind::Exec)
                .with_msg("scroll: Failed to set EV_REL"));
        }

        libc::ioctl(fd, UI_SET_RELBIT, REL_WHEEL as libc::c_int);
        libc::ioctl(fd, UI_SET_RELBIT, REL_HWHEEL as libc::c_int);
        libc::ioctl(fd, UI_SET_RELBIT, REL_WHEEL_HI_RES as libc::c_int);
        libc::ioctl(fd, UI_SET_RELBIT, REL_HWHEEL_HI_RES as libc::c_int);

        let mut setup: UinputSetup = mem::zeroed();
        setup.id = [0x06, 0x628, 0x1, 0x1];
        let name = b"Virtual Scroll Device";
        setup.name[..name.len()].copy_from_slice(name);

        libc::ioctl(fd, UI_DEV_SETUP, &setup);
        libc::ioctl(fd, UI_DEV_CREATE, 0);
    }

    std::thread::sleep(std::time::Duration::from_millis(200));
    Ok(uinput)
}
