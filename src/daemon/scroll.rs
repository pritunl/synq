use tokio_util::sync::CancellationToken;

use crate::errors::{error, info, trace};
use crate::errors::{Error, ErrorKind, Result};
use crate::config::Config;
use crate::scroll::{ScrollReceiver, ScrollBlocker, ScrollSender, ScrollSource, SharedUinput, resolve_devices};
use crate::transport::{Transport, ScrollInjectRx, ActiveState};
use crate::synq::{ScrollEvent, ScrollSource as ProtoScrollSource};

pub(crate) fn run_scroll_source(
    device_path: String,
    transport: Transport,
    scroll_reverse: bool,
    scroll_modifier: f64,
    cancel: CancellationToken,
) {
    let mut receiver = match ScrollReceiver::new(&device_path) {
        Ok(r) => r,
        Err(e) => {
            let e = Error::wrap(e, ErrorKind::Exec)
                .with_msg("daemon: Failed to create scroll receiver")
                .with_ctx("device", &device_path);
            error(&e);
            return;
        }
    };
    info!("Started scroll receiver on {}", device_path);

    while !cancel.is_cancelled() {
        match receiver.read_event() {
            Ok(Some(event)) => {
                let (delta_x, delta_y) = if scroll_reverse {
                    (-event.delta_x * scroll_modifier, -event.delta_y * scroll_modifier)
                } else {
                    (event.delta_x * scroll_modifier, event.delta_y * scroll_modifier)
                };

                trace!(
                    source = ?event.source,
                    delta_x = delta_x,
                    delta_y = delta_y,
                    "Scroll event",
                );

                let proto_source = match event.source {
                    ScrollSource::Wheel => ProtoScrollSource::Wheel,
                    ScrollSource::Finger => ProtoScrollSource::Finger,
                    ScrollSource::Continuous => ProtoScrollSource::Continuous,
                };

                let scroll_event = ScrollEvent {
                    source: proto_source.into(),
                    delta_x,
                    delta_y,
                };

                let _ = transport.send_scroll(scroll_event);
            }
            Ok(None) => {}
            Err(e) => {
                let e = Error::wrap(e, ErrorKind::Read)
                    .with_msg("daemon: Scroll receiver error")
                    .with_ctx("device", &device_path);
                error(&e);
                return;
            }
        }
    }
}

pub(crate) fn run_scroll_inject(
    rx: ScrollInjectRx,
    uinput: SharedUinput,
    transport: Transport,
) {
    let mut sender = ScrollSender::new(uinput, transport);
    info!("Started scroll sender");

    while let Some(event) = rx.recv() {
        if let Err(e) = sender.send(event.delta_x, event.delta_y) {
            let e = Error::wrap(e, ErrorKind::Exec)
                .with_msg("daemon: Failed to send scroll event");
            error(&e);
        }
    }
}

pub(crate) fn run_scroll_blocker(
    device_path: String,
    uinput: SharedUinput,
    active_state: ActiveState,
    transport: Transport,
    cancel: CancellationToken,
) {
    let on_scroll: Box<dyn Fn() + Send> = Box::new(move || {
        transport.send_activate_request();
    });

    let mut blocker = match ScrollBlocker::new(
        &device_path,
        uinput,
        active_state,
        Some(on_scroll),
    ) {
        Ok(b) => b,
        Err(e) => {
            let e = Error::wrap(e, ErrorKind::Exec)
                .with_msg("daemon: Failed to start scroll blocker")
                .with_ctx("device", &device_path);
            error(&e);
            return;
        }
    };

    info!("Started scroll blocker on {}", device_path);

    if let Err(e) = blocker.run(cancel) {
        let e = Error::wrap(e, ErrorKind::Exec)
            .with_msg("daemon: Scroll blocker error")
            .with_ctx("device", &device_path);
        error(&e);
    }
}

pub(crate) fn run_scroll_blockers(
    config: &Config,
    transport: Transport,
) -> Result<()> {
    let scroll_inject_rx = transport.take_scroll_inject_rx();

    let blocker_devices = resolve_devices(
        &config.server.scroll_input_devices)?;

    let first_device_path = blocker_devices.first()
        .map(|d| d.path.clone())
        .ok_or_else(|| Error::new(ErrorKind::Invalid)
            .with_msg("daemon: No scroll input devices configured"))?;

    let source_file = std::fs::OpenOptions::new()
        .read(true)
        .open(&first_device_path)
        .map_err(|e| Error::wrap(e, ErrorKind::Read)
            .with_msg("daemon: Failed to open scroll device for uinput setup")
            .with_ctx("path", &first_device_path))?;
    let source_fd = std::os::unix::io::AsRawFd::as_raw_fd(&source_file);

    let shared_uinput = SharedUinput::new(source_fd)
        .map_err(|e| Error::wrap(e, ErrorKind::Exec)
            .with_msg("daemon: Failed to create shared uinput device"))?;

    drop(source_file);

    if let Some(rx) = scroll_inject_rx {
        tokio::task::spawn_blocking({
            let shared_uinput = shared_uinput.clone();
            let transport = transport.clone();

            move || {
                run_scroll_inject(rx, shared_uinput, transport);
            }
        });
    }

    let cancel = transport.cancel_token();
    for device in blocker_devices {
        let blocker_cancel = cancel.clone();
        let blocker_active_state = transport.active_state().clone();
        let blocker_transport = transport.clone();
        let blocker_uinput = shared_uinput.clone();
        tokio::task::spawn_blocking(move || {
            run_scroll_blocker(
                device.path,
                blocker_uinput,
                blocker_active_state,
                blocker_transport,
                blocker_cancel,
            );
        });
    }

    Ok(())
}
