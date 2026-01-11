use std::fmt;
use std::fs::{File, OpenOptions};
use std::os::unix::fs::OpenOptionsExt;
use std::os::unix::io::OwnedFd;
use std::path::Path;

use input::event::EventTrait;
use input::{DeviceCapability, Libinput, LibinputInterface, ScrollMethod};
use libc::{O_ACCMODE, O_RDONLY, O_RDWR, O_WRONLY};

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

pub fn resolve_devices(config_values: &[String]) -> Result<Vec<String>> {
    let devices = list_devices()?;
    let mut resolved = Vec::new();

    for value in config_values {
        let is_path = value.starts_with('/');

        let matched = devices.iter().find(|d| {
            if is_path {
                d.path == *value
            } else {
                d.name.eq_ignore_ascii_case(value)
            }
        });

        if let Some(device) = matched {
            resolved.push(device.path.clone());
        }
    }

    Ok(resolved)
}
