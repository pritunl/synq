use std::sync::Arc;
use std::sync::atomic::Ordering;

use tokio::time::{sleep, Duration};
use tokio_util::sync::CancellationToken;

use crate::errors::{error, info, warn, trace};
use crate::errors::{Result, Error, ErrorKind};
use crate::config::Config;
use crate::crypto::KeyStore;
use crate::clipboard;
use crate::scroll::{self, ScrollReceiver, ScrollBlocker, ScrollSender, ScrollSource};
use crate::transport::{Transport, ScrollInjectRx, ActiveState, send_active_state};
use crate::utils;
use crate::synq::{ScrollEvent, ScrollSource as ProtoScrollSource};

use super::constants::CLIPBOARD_TTL;

fn run_scroll_source(
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

fn run_scroll_inject(rx: ScrollInjectRx) {
    let mut sender = match ScrollSender::new() {
        Ok(s) => s,
        Err(e) => {
            let e = Error::wrap(e, ErrorKind::Exec)
                .with_msg("daemon: Failed to create scroll sender");
            error(&e);
            return;
        }
    };
    info!("Started scroll sender");

    while let Some(event) = rx.recv() {
        if let Err(e) = sender.send(event.delta_x, event.delta_y) {
            let e = Error::wrap(e, ErrorKind::Exec)
                .with_msg("daemon: Failed to send scroll event");
            error(&e);
        }
    }
}

fn run_scroll_blocker(
    device_path: String,
    active_state: ActiveState,
    transport: Transport,
    cancel: CancellationToken,
) {
    let on_scroll: Box<dyn Fn() + Send> = Box::new(move || {
        transport.send_active_request();
    });

    let mut blocker = match ScrollBlocker::new(
        &device_path,
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

async fn run_clipboard_source(
    config: Config,
    transport: Transport,
    cancel: CancellationToken,
) {
    info!("Starting clipboard source");

    let mut clipboard_rx = match clipboard::watch_clipboard().await {
        Ok(rx) => rx,
        Err(e) => {
            let e = Error::wrap(e, ErrorKind::Exec)
                .with_msg("daemon: Failed to start clipboard watcher");
            error(&e);
            return;
        }
    };

    loop {
        tokio::select! {
            _ = cancel.cancelled() => break,
            change = clipboard_rx.recv() => {
                if change.is_none() {
                    break;
                }
            }
        };

        let last_set = transport.last_set_clipboard().load(Ordering::SeqCst);
        if utils::mono_time_ms().saturating_sub(last_set) < CLIPBOARD_TTL {
            trace!("Ignoring clipboard change within debounce window");
            continue;
        }

        let clipboard_text = match clipboard::get_clipboard().await {
            Ok(text) => text,
            Err(e) => {
                let e = Error::wrap(e, ErrorKind::Read)
                    .with_msg("daemon: Failed to get clipboard");
                warn!(?e);
                continue;
            }
        };

        trace!("Clipboard changed, sending to peers");

        for peer in &config.peers {
            if peer.clipboard_destination {
                if !transport.send_clipboard(
                    peer.address.clone(),
                    peer.public_key.clone(),
                    clipboard_text.clone(),
                ) {
                    warn!("Clipboard send dropped for {}", peer.address);
                }
            }
        }
    }
}

pub async fn run(config: Config) -> Result<()> {
    let should_run_server = config.server.clipboard_destination || config.server.scroll_destination;
    let should_run_clipboard_source = config.server.clipboard_source;
    let should_run_scroll_source = config.server.scroll_source;

    utils::mono_time_ms();

    info!(
        "Daemon starting: server={} clipboard_source={} scroll_source={}",
        should_run_server, should_run_clipboard_source, should_run_scroll_source,
    );

    if !should_run_server && !should_run_clipboard_source && !should_run_scroll_source {
        return Err(Error::new(ErrorKind::Invalid)
            .with_msg("daemon: No services configured"));
    }

    let key_store = Arc::new(
        KeyStore::new(&config.server.private_key)
        .map_err(|e| Error::wrap(e, ErrorKind::Exec)
            .with_msg("daemon: Failed to create key store"))?);

    let (transport, scroll_inject_rx) = Transport::new(&config, key_store).await?;
    let cancel = transport.cancel_token();

    if should_run_scroll_source {
        let resolved_devices = scroll::resolve_devices(&config.server.scroll_input_devices)?;

        let host_key = config.server.public_key.clone();
        for peer in &config.peers {
            if peer.scroll_destination {
                let address = peer.address.clone();
                let peer_key = host_key.clone();
                tokio::spawn(async move {
                    trace!(
                        peer = %address,
                        "Send state reset",
                    );
                    if let Err(e) = send_active_state(&address, &peer_key, 0).await {
                        error(&e);
                    }
                });
            }
        }

        for device in resolved_devices {
            let transport = transport.clone();
            let device_cancel = cancel.clone();
            tokio::task::spawn_blocking(move || {
                run_scroll_source(
                    device.path,
                    transport,
                    device.scroll_reverse,
                    device.scroll_modifier,
                    device_cancel,
                );
            });
        }
    }

    if let Some(rx) = scroll_inject_rx {
        tokio::task::spawn_blocking(move || {
            run_scroll_inject(rx);
        });
    }

    if config.server.scroll_destination {
        let blocker_devices = scroll::resolve_devices(
            &config.server.scroll_input_devices)?;
        for device in blocker_devices {
            let blocker_cancel = cancel.clone();
            let blocker_active_state = transport.active_state().clone();
            let blocker_transport = transport.clone();
            tokio::task::spawn_blocking(move || {
                run_scroll_blocker(
                    device.path,
                    blocker_active_state,
                    blocker_transport,
                    blocker_cancel,
                );
            });
        }
    }

    if should_run_clipboard_source {
        let clipboard_config = config.clone();
        let clipboard_transport = transport.clone();
        let clipboard_cancel = cancel.clone();
        tokio::spawn(async move {
            run_clipboard_source(
                clipboard_config,
                clipboard_transport,
                clipboard_cancel,
            ).await;
        });
    }

    let mut sigterm = tokio::signal::unix::signal(
        tokio::signal::unix::SignalKind::terminate(),
    ).map_err(|e| Error::wrap(e, ErrorKind::Read)
        .with_msg("daemon: Failed to register SIGTERM handler"))?;

    tokio::select! {
        _ = tokio::signal::ctrl_c() => {
            info!("Received SIGINT, shutting down");
        }
        _ = sigterm.recv() => {
            info!("Received SIGTERM, shutting down");
        }
    }

    transport.shutdown();
    sleep(Duration::from_millis(500)).await;
    std::process::exit(0)
}
