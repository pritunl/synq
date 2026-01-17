use std::fs::{File, OpenOptions};
use std::io::Write;
use std::mem;
use std::os::unix::io::{AsRawFd, RawFd};
use std::sync::{Arc, Mutex};

use crate::errors::{Error, ErrorKind, Result};
use super::event::InputEvent;
use super::constants::{
    UI_SET_EVBIT,
    UI_SET_KEYBIT,
    UI_SET_RELBIT,
    UI_SET_ABSBIT,
    UI_SET_MSCBIT,
    UI_SET_LEDBIT,
    UI_SET_SNDBIT,
    UI_SET_FFBIT,
    UI_SET_SWBIT,
    UI_ABS_SETUP,
    UI_DEV_SETUP,
    UI_DEV_CREATE,
    EV_KEY,
    EV_REL,
    EV_ABS,
    EV_MSC,
    EV_SW,
    EV_LED,
    EV_SND,
    EV_FF,
    EV_MAX,
    KEY_MAX,
    REL_MAX,
    ABS_MAX,
    MSC_MAX,
    SW_MAX,
    LED_MAX,
    SND_MAX,
    FF_MAX,
    REL_WHEEL,
    REL_HWHEEL,
    REL_WHEEL_HI_RES,
    REL_HWHEEL_HI_RES,
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

fn bit_is_set(bits: &[u8], bit: usize) -> bool {
    let byte_idx = bit / 8;
    let bit_idx = bit % 8;
    byte_idx < bits.len() && (bits[byte_idx] & (1 << bit_idx)) != 0
}

const fn eviocgbit(ev: u32, len: u32) -> libc::c_ulong {
    (2u64 << 30) | (((len as u64) & 0x3fff) << 16) | (0x45u64 << 8) | (0x20 + ev) as u64
}

const fn eviocgabs(abs: u32) -> libc::c_ulong {
    (2u64 << 30) | (24u64 << 16) | (0x45u64 << 8) | (0x40 + abs) as u64
}

pub(crate) fn setup_uinput_merged(source_fd: RawFd) -> Result<File> {
    let uinput = OpenOptions::new()
        .read(true)
        .write(true)
        .open("/dev/uinput")
        .map_err(|e| {
            Error::wrap(e, ErrorKind::Read)
                .with_msg("scroll: Failed to open uinput device")
        })?;

    let ufd = uinput.as_raw_fd();

    unsafe {
        let mut ev_bits = [0u8; (EV_MAX + 7) / 8 + 1];
        if libc::ioctl(
            source_fd,
            eviocgbit(0, ev_bits.len() as u32),
            ev_bits.as_mut_ptr(),
        ) < 0 {
            return Err(Error::wrap(std::io::Error::last_os_error(), ErrorKind::Exec)
                .with_msg("scroll: Failed to get event types from source device"));
        }

        if bit_is_set(&ev_bits, EV_KEY as usize) {
            libc::ioctl(ufd, UI_SET_EVBIT, EV_KEY);
            let mut key_bits = [0u8; (KEY_MAX + 7) / 8 + 1];
            if libc::ioctl(
                source_fd,
                eviocgbit(EV_KEY as u32, key_bits.len() as u32),
                key_bits.as_mut_ptr(),
            ) >= 0 {
                for code in 0..=KEY_MAX {
                    if bit_is_set(&key_bits, code) {
                        libc::ioctl(ufd, UI_SET_KEYBIT, code as libc::c_int);
                    }
                }
            }
        }

        libc::ioctl(ufd, UI_SET_EVBIT, EV_REL);
        if bit_is_set(&ev_bits, EV_REL as usize) {
            let mut rel_bits = [0u8; (REL_MAX + 7) / 8 + 1];
            if libc::ioctl(
                source_fd,
                eviocgbit(EV_REL as u32, rel_bits.len() as u32),
                rel_bits.as_mut_ptr(),
            ) >= 0 {
                for code in 0..=REL_MAX {
                    if bit_is_set(&rel_bits, code) {
                        libc::ioctl(ufd, UI_SET_RELBIT, code as libc::c_int);
                    }
                }
            }
        }
        libc::ioctl(ufd, UI_SET_RELBIT, REL_WHEEL as libc::c_int);
        libc::ioctl(ufd, UI_SET_RELBIT, REL_HWHEEL as libc::c_int);
        libc::ioctl(ufd, UI_SET_RELBIT, REL_WHEEL_HI_RES as libc::c_int);
        libc::ioctl(ufd, UI_SET_RELBIT, REL_HWHEEL_HI_RES as libc::c_int);

        if bit_is_set(&ev_bits, EV_ABS as usize) {
            libc::ioctl(ufd, UI_SET_EVBIT, EV_ABS);
            let mut abs_bits = [0u8; (ABS_MAX + 7) / 8 + 1];
            if libc::ioctl(
                source_fd,
                eviocgbit(EV_ABS as u32, abs_bits.len() as u32),
                abs_bits.as_mut_ptr(),
            ) >= 0 {
                for code in 0..=ABS_MAX {
                    if bit_is_set(&abs_bits, code) {
                        libc::ioctl(ufd, UI_SET_ABSBIT, code as libc::c_int);

                        let mut absinfo: AbsInfo = mem::zeroed();
                        if libc::ioctl(source_fd, eviocgabs(code as u32), &mut absinfo) >= 0 {
                            let abs_setup = UinputAbsSetup {
                                code: code as u16,
                                _padding: 0,
                                absinfo,
                            };
                            libc::ioctl(ufd, UI_ABS_SETUP, &abs_setup);
                        }
                    }
                }
            }
        }

        if bit_is_set(&ev_bits, EV_MSC as usize) {
            libc::ioctl(ufd, UI_SET_EVBIT, EV_MSC);
            let mut msc_bits = [0u8; (MSC_MAX + 7) / 8 + 1];
            if libc::ioctl(
                source_fd,
                eviocgbit(EV_MSC as u32, msc_bits.len() as u32),
                msc_bits.as_mut_ptr(),
            ) >= 0 {
                for code in 0..=MSC_MAX {
                    if bit_is_set(&msc_bits, code) {
                        libc::ioctl(ufd, UI_SET_MSCBIT, code as libc::c_int);
                    }
                }
            }
        }

        if bit_is_set(&ev_bits, EV_SW as usize) {
            libc::ioctl(ufd, UI_SET_EVBIT, EV_SW);
            let mut sw_bits = [0u8; (SW_MAX + 7) / 8 + 1];
            if libc::ioctl(
                source_fd,
                eviocgbit(EV_SW as u32, sw_bits.len() as u32),
                sw_bits.as_mut_ptr(),
            ) >= 0 {
                for code in 0..=SW_MAX {
                    if bit_is_set(&sw_bits, code) {
                        libc::ioctl(ufd, UI_SET_SWBIT, code as libc::c_int);
                    }
                }
            }
        }

        if bit_is_set(&ev_bits, EV_LED as usize) {
            libc::ioctl(ufd, UI_SET_EVBIT, EV_LED);
            let mut led_bits = [0u8; (LED_MAX + 7) / 8 + 1];
            if libc::ioctl(
                source_fd,
                eviocgbit(EV_LED as u32, led_bits.len() as u32),
                led_bits.as_mut_ptr(),
            ) >= 0 {
                for code in 0..=LED_MAX {
                    if bit_is_set(&led_bits, code) {
                        libc::ioctl(ufd, UI_SET_LEDBIT, code as libc::c_int);
                    }
                }
            }
        }

        if bit_is_set(&ev_bits, EV_SND as usize) {
            libc::ioctl(ufd, UI_SET_EVBIT, EV_SND);
            let mut snd_bits = [0u8; (SND_MAX + 7) / 8 + 1];
            if libc::ioctl(
                source_fd,
                eviocgbit(EV_SND as u32, snd_bits.len() as u32),
                snd_bits.as_mut_ptr(),
            ) >= 0 {
                for code in 0..=SND_MAX {
                    if bit_is_set(&snd_bits, code) {
                        libc::ioctl(ufd, UI_SET_SNDBIT, code as libc::c_int);
                    }
                }
            }
        }

        if bit_is_set(&ev_bits, EV_FF as usize) {
            libc::ioctl(ufd, UI_SET_EVBIT, EV_FF);
            let mut ff_bits = [0u8; (FF_MAX + 7) / 8 + 1];
            if libc::ioctl(
                source_fd,
                eviocgbit(EV_FF as u32, ff_bits.len() as u32),
                ff_bits.as_mut_ptr(),
            ) >= 0 {
                for code in 0..=FF_MAX {
                    if bit_is_set(&ff_bits, code) {
                        libc::ioctl(ufd, UI_SET_FFBIT, code as libc::c_int);
                    }
                }
            }
        }

        let mut setup: UinputSetup = mem::zeroed();
        setup.id = SCROLL_DEVICE_ID;
        setup.name[..SCROLL_DEVICE_NAME.len()].copy_from_slice(SCROLL_DEVICE_NAME);

        libc::ioctl(ufd, UI_DEV_SETUP, &setup);
        libc::ioctl(ufd, UI_DEV_CREATE, 0);
    }

    std::thread::sleep(std::time::Duration::from_millis(200));
    Ok(uinput)
}

#[derive(Clone)]
pub struct SharedUinput {
    inner: Arc<Mutex<File>>,
}

impl SharedUinput {
    pub fn new(source_fd: RawFd) -> Result<Self> {
        let file = setup_uinput_merged(source_fd)?;
        Ok(Self {
            inner: Arc::new(Mutex::new(file)),
        })
    }

    pub fn write_event(&self, event: &InputEvent) -> Result<()> {
        let bytes: [u8; mem::size_of::<InputEvent>()] = unsafe { mem::transmute(*event) };
        let mut guard = self.inner.lock().map_err(|_| {
            Error::new(ErrorKind::Exec)
                .with_msg("scroll: Failed to acquire uinput lock")
        })?;
        (&mut *guard).write_all(&bytes).map_err(|e| {
            Error::wrap(e, ErrorKind::Write)
                .with_msg("scroll: Failed to write event to uinput")
        })
    }
}
