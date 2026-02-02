use std::sync::Arc;

use tokio::time::{sleep, Duration};

use crate::errors::{error, info, trace};
use crate::errors::{Result, Error, ErrorKind};
use crate::config::Config;
use crate::crypto::KeyStore;
use crate::transport::{Transport, send_active_state};
use crate::utils;

use super::monitor::run_scroll_source_monitor;
use super::scroll::run_scroll_blockers;
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

    let transport = Transport::new(&config, key_store).await?;

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

        tokio::task::spawn_blocking({
            let config = config.clone();
            let transport = transport.clone();

            move || {
                run_scroll_source_monitor(config, transport);
            }
        });
    }

    if config.server.scroll_destination {
        run_scroll_blockers(&config, transport.clone()).await?;
    }

    if should_run_clipboard_source {
        tokio::spawn({
            let config = config.clone();
            let transport = transport.clone();

            async move {
                run_clipboard_source(config, transport).await;
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
