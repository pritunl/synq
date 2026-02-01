use std::sync::Arc;

use tokio::time::{sleep, Duration};

use crate::errors::{error, info, warn, trace};
use crate::errors::{Result, Error, ErrorKind};
use crate::config::Config;
use crate::crypto::KeyStore;
use crate::scroll::{SharedUinput, resolve_devices};
use crate::transport::{Transport, send_active_state};
use crate::utils;

use super::monitor::run_scroll_source_monitor;
use super::scroll::{run_scroll_blocker, run_scroll_inject};
use super::clipboard::run_clipboard_source;

pub async fn run(config: Config) -> Result<()> {
    let should_run_server = config.server.clipboard_destination
        || config.server.scroll_destination;
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

        let input_devices = config.server.scroll_input_devices.clone();
        let monitor_transport = transport.clone();
        let monitor_cancel = cancel.clone();
        tokio::task::spawn_blocking(move || {
            run_scroll_source_monitor(input_devices, monitor_transport, monitor_cancel);
        });
    }

    if config.server.scroll_destination {
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
            let inject_uinput = shared_uinput.clone();
            let inject_transport = transport.clone();
            tokio::task::spawn_blocking(move || {
                run_scroll_inject(rx, inject_uinput, inject_transport);
            });
        }

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
    } else if let Some(rx) = scroll_inject_rx {
        warn!("Scroll inject receiver exists but scroll_destination is disabled");
        drop(rx);
    }

    if should_run_clipboard_source {
        tokio::spawn({
            let config = config.clone();
            let transport = transport.clone();

            async move {
                run_clipboard_source(
                    config,
                    transport,
                ).await;
            }
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
