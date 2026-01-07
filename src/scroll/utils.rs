use std::fs::{File, OpenOptions};
use std::mem;
use std::os::unix::io::AsRawFd;

use crate::errors::{Error, ErrorKind, Result};
use super::constants::{
    UI_SET_EVBIT,
    UI_SET_KEYBIT,
    UI_SET_RELBIT,
    UI_SET_ABSBIT,
    UI_ABS_SETUP,
    UI_DEV_SETUP,
    UI_DEV_CREATE,
    EV_KEY,
    EV_REL,
    EV_ABS,
    REL_WHEEL,
    REL_HWHEEL,
    REL_WHEEL_HI_RES,
    REL_HWHEEL_HI_RES,
    VIRTIO_TABLET_NAME,
    VIRTIO_TABLET_ID,
    SCROLL_DEVICE_NAME,
    SCROLL_DEVICE_ID,
};

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

pub(crate) fn setup_uinput_virtio() -> Result<File> {
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
        setup.id = VIRTIO_TABLET_ID;
        setup.name[..VIRTIO_TABLET_NAME.len()].copy_from_slice(VIRTIO_TABLET_NAME);

        libc::ioctl(fd, UI_DEV_SETUP, &setup);
        libc::ioctl(fd, UI_DEV_CREATE, 0);
    }

    std::thread::sleep(std::time::Duration::from_millis(200));
    Ok(uinput)
}

pub(crate) fn setup_uinput_scroll() -> Result<File> {
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
        setup.id = SCROLL_DEVICE_ID;
        setup.name[..SCROLL_DEVICE_NAME.len()].copy_from_slice(SCROLL_DEVICE_NAME);

        libc::ioctl(fd, UI_DEV_SETUP, &setup);
        libc::ioctl(fd, UI_DEV_CREATE, 0);
    }

    std::thread::sleep(std::time::Duration::from_millis(200));
    Ok(uinput)
}
