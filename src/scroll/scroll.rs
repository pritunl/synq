use std::fs::{File, OpenOptions};
use std::io::{Read, Write};
use std::mem;
use std::os::unix::io::AsRawFd;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use tracing::info;

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
            info!(code = event.code, value = event.value, "Blocked scroll event");
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

    pub fn run_blocking(&mut self) -> Result<()> {
        loop {
            self.process_events()?;
        }
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

    pub fn send(&mut self, delta_x: i32, delta_y: i32) -> Result<()> {
        let now = unsafe {
            let mut tv: libc::timeval = mem::zeroed();
            libc::gettimeofday(&mut tv, std::ptr::null_mut());
            tv
        };

        if delta_y != 0 {
            let event = InputEvent {
                tv_sec: now.tv_sec,
                tv_usec: now.tv_usec,
                type_: EV_REL as u16,
                code: REL_WHEEL,
                value: delta_y,
            };
            self.write_event(&event)?;
        }

        if delta_x != 0 {
            let event = InputEvent {
                tv_sec: now.tv_sec,
                tv_usec: now.tv_usec,
                type_: EV_REL as u16,
                code: REL_HWHEEL,
                value: delta_x,
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
        let bytes: [u8; mem::size_of::<InputEvent>()] = unsafe {
            mem::transmute(*event)
        };
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
        .map_err(|e| Error::wrap(e, ErrorKind::Read)
            .with_msg("scroll: Failed to open uinput device"))?;

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
        .map_err(|e| Error::wrap(e, ErrorKind::Read)
            .with_msg("scroll: Failed to open uinput device"))?;

    let fd = uinput.as_raw_fd();

    unsafe {
        if libc::ioctl(fd, UI_SET_EVBIT, EV_REL) < 0 {
            return Err(Error::wrap(std::io::Error::last_os_error(), ErrorKind::Exec)
                .with_msg("scroll: Failed to set EV_REL"));
        }

        libc::ioctl(fd, UI_SET_RELBIT, REL_WHEEL as libc::c_int);
        libc::ioctl(fd, UI_SET_RELBIT, REL_HWHEEL as libc::c_int);

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

