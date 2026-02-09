use std::collections::HashMap;
use std::fs::{File, OpenOptions};
use std::os::unix::fs::OpenOptionsExt;
use std::os::unix::io::{AsRawFd, OwnedFd};
use std::path::Path;

use input::{Libinput, LibinputInterface, Event as LibinputEvent};
use input::event::{DeviceEvent, EventTrait};
use libc::{O_ACCMODE, O_RDONLY, O_RDWR, O_WRONLY};
use tokio_util::sync::CancellationToken;

use crate::config::{Config, InputDevice};
use crate::errors::{Error, ErrorKind, error, info};
use crate::transport::Transport;

use super::scroll::run_scroll_source;

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

struct ActiveReceiver {
    cancel: CancellationToken,
}

pub(crate) fn run_scroll_source_monitor(
    config: Config,
    transport: Transport,
) {
    let mut libinput = Libinput::new_with_udev(Interface);
    if libinput.udev_assign_seat("seat0").is_err() {
        let e = Error::new(ErrorKind::Exec)
            .with_msg("daemon: Failed to assign udev seat");
        error(&e);
        return;
    }

    let fd = libinput.as_raw_fd();
    let mut active_receivers: HashMap<String, ActiveReceiver> = HashMap::new();
    let cancel = transport.cancel_token();
    let input_devices = config.server.scroll_input_devices.clone();

    let matches_config = |name: &str| -> Option<&InputDevice> {
        input_devices.iter().find(|d| {
            d.name.as_ref().is_some_and(|n| n.eq_ignore_ascii_case(name))
        })
    };

    info!("Started scroll source monitor");

    loop {
        if cancel.is_cancelled() {
            break;
        }

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
            let e = Error::wrap(err, ErrorKind::Read)
                .with_msg("daemon: Poll failed in scroll source monitor");
            error(&e);
            return;
        }

        if ret == 0 {
            continue;
        }

        if let Err(e) = libinput.dispatch() {
            let e = Error::wrap(e, ErrorKind::Read)
                .with_msg("daemon: Failed to dispatch libinput events");
            error(&e);
            return;
        }

        for event in &mut libinput {
            match event {
                LibinputEvent::Device(DeviceEvent::Added(evt)) => {
                    let device = evt.device();
                    let name = device.name().to_string();
                    let path = format!("/dev/input/{}", device.sysname());

                    if let Some(config) = matches_config(&name) {
                        if active_receivers.contains_key(&name) {
                            continue;
                        }

                        info!("Scroll device connected: {} ({})", name, path);

                        let cancel = CancellationToken::new();

                        std::thread::spawn({
                            let path = path.clone();
                            let transport = transport.clone();
                            let scroll_reverse = config.scroll_reverse;
                            let scroll_modifier = config.scroll_modifier;
                            let cancel = cancel.clone();

                            move || {
                                run_scroll_source(
                                    path,
                                    transport,
                                    scroll_reverse,
                                    scroll_modifier,
                                    cancel,
                                );
                            }
                        });

                        active_receivers.insert(name, ActiveReceiver {
                            cancel,
                        });
                    }
                }
                LibinputEvent::Device(DeviceEvent::Removed(evt)) => {
                    let device = evt.device();
                    let name = device.name().to_string();

                    if let Some(receiver) = active_receivers.remove(&name) {
                        info!("Scroll device disconnected: {}", name);
                        receiver.cancel.cancel();
                    }
                }
                _ => {}
            }
        }
    }

    for (_, receiver) in active_receivers {
        receiver.cancel.cancel();
    }
}
