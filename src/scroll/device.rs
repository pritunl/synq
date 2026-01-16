use std::fmt;
use std::fs::{File, OpenOptions};
use std::os::unix::fs::OpenOptionsExt;
use std::os::unix::io::{AsRawFd, OwnedFd};
use std::path::Path;

use input::event::pointer::PointerEvent;
use input::event::EventTrait;
use input::{DeviceCapability, Event, Libinput, LibinputInterface, ScrollMethod};
use libc::{O_ACCMODE, O_RDONLY, O_RDWR, O_WRONLY};

use crate::config::{Config, InputDevice};
use crate::errors::{Error, ErrorKind, Result};

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

#[derive(Debug, Clone)]
pub struct Device {
    pub name: String,
    pub path: String,
    pub capabilities: Vec<DeviceCapability>,
    pub scroll_methods: Vec<ScrollMethod>,
    pub has_scroll: bool,
}

impl fmt::Display for Device {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(f, "Device: {}", self.name)?;
        writeln!(f, "  Path: {}", self.path)?;

        let caps: Vec<&str> = self
            .capabilities
            .iter()
            .map(|c| match c {
                DeviceCapability::Keyboard => "keyboard",
                DeviceCapability::Pointer => "pointer",
                DeviceCapability::Touch => "touch",
                DeviceCapability::TabletTool => "tablet-tool",
                DeviceCapability::TabletPad => "tablet-pad",
                DeviceCapability::Gesture => "gesture",
                DeviceCapability::Switch => "switch",
                _ => "unknown",
            })
            .collect();
        writeln!(f, "  Capabilities: {}", caps.join(" "))?;

        let methods: Vec<&str> = self
            .scroll_methods
            .iter()
            .map(|m| match m {
                ScrollMethod::NoScroll => "none",
                ScrollMethod::TwoFinger => "two-finger",
                ScrollMethod::Edge => "edge",
                ScrollMethod::OnButtonDown => "button",
                _ => "unknown",
            })
            .collect();
        writeln!(f, "  Has Scroll: {}", self.has_scroll)?;
        write!(f, "  Scroll methods: {}", methods.join(" "))
    }
}

pub fn list_devices() -> Result<Vec<Device>> {
    let mut libinput = Libinput::new_with_udev(Interface);
    libinput.udev_assign_seat("seat0").map_err(|_| {
        Error::new(ErrorKind::Read).with_msg("device: Failed to assign udev seat")
    })?;

    libinput.dispatch().map_err(|e| {
        Error::wrap(e, ErrorKind::Read).with_msg("device: Failed to dispatch libinput")
    })?;

    let mut devices = Vec::new();
    for event in &mut libinput {
        if let input::Event::Device(input::event::DeviceEvent::Added(event)) = event {
            let dev = event.device();

            let capabilities = [
                DeviceCapability::Keyboard,
                DeviceCapability::Pointer,
                DeviceCapability::Touch,
                DeviceCapability::TabletTool,
                DeviceCapability::TabletPad,
                DeviceCapability::Gesture,
                DeviceCapability::Switch,
            ]
            .into_iter()
            .filter(|cap| dev.has_capability(*cap))
            .collect();

            let scroll_methods = dev.config_scroll_methods();

            let has_scroll = scroll_methods.iter().any(|m| {
                matches!(
                    m,
                    ScrollMethod::TwoFinger | ScrollMethod::Edge | ScrollMethod::OnButtonDown,
                )
            });

            devices.push(Device {
                name: dev.name().to_string(),
                path: format!("/dev/input/{}", dev.sysname()),
                capabilities,
                scroll_methods,
                has_scroll,
            });
        }
    }

    Ok(devices)
}

pub struct ResolvedDevice {
    pub path: String,
    pub scroll_reverse: bool,
    pub scroll_modifier: f64,
}

pub fn resolve_devices(input_devices: &[InputDevice]) -> Result<Vec<ResolvedDevice>> {
    let devices = list_devices()?;
    let mut resolved = Vec::new();

    for input in input_devices {
        let matches_input = |d: &Device| {
            let path_matches = input.path.as_ref().is_some_and(|p| d.path == *p);
            let name_matches = input.name.as_ref().is_some_and(|n| d.name.eq_ignore_ascii_case(n));
            path_matches || name_matches
        };

        let matched: Vec<_> = devices
            .iter()
            .filter(|d| d.has_scroll && matches_input(d))
            .collect();

        let matched = if matched.is_empty() {
            devices.iter().filter(|d| matches_input(d)).collect()
        } else {
            matched
        };

        for device in matched {
            resolved.push(ResolvedDevice {
                path: device.path.clone(),
                scroll_reverse: input.scroll_reverse,
                scroll_modifier: input.scroll_modifier,
            });
        }
    }

    Ok(resolved)
}

pub async fn detect_scroll_devices(mut config: Config) -> Result<()> {
    println!("Listening for scroll events... Press Ctrl+C to stop.");
    println!();

    let mut libinput = Libinput::new_with_udev(Interface);
    libinput.udev_assign_seat("seat0").map_err(|_| {
        Error::new(ErrorKind::Read).with_msg("device: Failed to assign udev seat")
    })?;

    let fd = libinput.as_raw_fd();
    let mut detected_devices: Vec<InputDevice> = Vec::new();

    loop {
        let mut pfd = libc::pollfd {
            fd,
            events: libc::POLLIN,
            revents: 0,
        };

        let ret = unsafe { libc::poll(&mut pfd, 1, 100) };
        if ret < 0 {
            let err = std::io::Error::last_os_error();
            if err.kind() == std::io::ErrorKind::Interrupted {
                continue;
            }
            return Err(Error::wrap(err, ErrorKind::Read)
                .with_msg("device: Poll failed"));
        }

        if ret == 0 {
            continue;
        }

        libinput.dispatch().map_err(|e| {
            Error::wrap(e, ErrorKind::Read).with_msg("device: Failed to dispatch libinput")
        })?;

        for event in &mut libinput {
            if let Event::Pointer(pointer_event) = event {
                let is_scroll = matches!(
                    pointer_event,
                    PointerEvent::ScrollWheel(_)
                        | PointerEvent::ScrollFinger(_)
                        | PointerEvent::ScrollContinuous(_)
                );

                if is_scroll {
                    let device = match &pointer_event {
                        PointerEvent::ScrollWheel(e) => e.device(),
                        PointerEvent::ScrollFinger(e) => e.device(),
                        PointerEvent::ScrollContinuous(e) => e.device(),
                        _ => continue,
                    };

                    let device_name = device.name().to_string();

                    if detected_devices.iter().any(|d| d.name.as_ref() == Some(&device_name)) {
                        continue;
                    }

                    println!("Detected scroll from: {}", device_name);

                    detected_devices.push(InputDevice {
                        name: Some(device_name),
                        path: None,
                        scroll_reverse: true,
                    });

                    config.server.scroll_input_devices = detected_devices.clone();
                    config.save().await?;

                    println!("Saved {} device(s) to config", detected_devices.len());
                    println!();
                }
            }
        }
    }
}
