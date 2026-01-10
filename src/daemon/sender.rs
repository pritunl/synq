use std::sync::Arc;
use std::sync::atomic::{AtomicI32, AtomicU64, Ordering};

use tracing::{error, info, warn, trace};
use tokio::sync::mpsc;
use tokio::time::{sleep, Duration};
use tokio_stream::wrappers::ReceiverStream;
use tokio_stream::StreamExt;
use tokio_util::sync::CancellationToken;
use tonic::transport::Channel;

use crate::errors::{Result, Error, ErrorKind};
use crate::config::Config;
use crate::crypto;
use crate::crypto::KeyStore;
use crate::clipboard;
use crate::scroll;
use crate::scroll::{ScrollReceiver, ScrollBlocker, ScrollSource};
use crate::utils;
use crate::synq::{
    synq_service_client::SynqServiceClient,
    ScrollEvent, ClipboardEvent, ScrollSource as ProtoScrollSource,
};
use super::receiver::DaemonReceiver;
use super::constants::CLIPBOARD_TTL;

pub(crate) struct DaemonSender {}

impl DaemonSender {
    async fn connect(address: &str) -> Result<SynqServiceClient<Channel>> {
        let host_port = match address.find('@') {
            Some(i) => &address[i + 1..],
            None => address,
        };

        let url = format!("http://{}", host_port);

        let channel = Channel::from_shared(url)
            .map_err(|e| Error::wrap(e, ErrorKind::Network)
                .with_msg("daemon: Invalid peer address")
                .with_ctx("address", address))?
            .connect()
            .await
            .map_err(|e| Error::wrap(e, ErrorKind::Network)
                .with_msg("daemon: Failed to connect to peer")
                .with_ctx("address", address))?;

        Ok(SynqServiceClient::new(channel))
    }

    async fn send_clipboard(
        config: &Config,
        key_store: &KeyStore,
        peer_address: &str,
        peer_public_key: &str,
        clipboard_text: &str,
    ) -> Result<()> {
        let encrypted = crypto::encrypt(
            key_store,
            peer_public_key,
            clipboard_text,
        )?;

        let event = ClipboardEvent {
            client: config.server.public_key.clone(),
            data: encrypted.into_bytes(),
        };

        let mut client = DaemonSender::connect(peer_address).await?;

        let (tx, rx) = mpsc::channel(1);
        tx.send(event).await
            .map_err(|e| Error::wrap(e, ErrorKind::Network)
                .with_msg("daemon: Failed to queue clipboard event"))?;
        drop(tx);

        let out_stream = ReceiverStream::new(rx);

        match client.clipboard(out_stream).await {
            Ok(response) => {
                let mut in_stream = response.into_inner();
                while let Some(result) = in_stream.next().await {
                    if let Err(e) = result {
                        let e = Error::wrap(e, ErrorKind::Network)
                            .with_msg("daemon: Clipboard stream error");
                        error!(?e);
                        break;
                    }
                }
            }
            Err(e) => {
                let e = Error::wrap(e, ErrorKind::Network)
                    .with_msg("daemon: Failed to send clipboard")
                    .with_ctx("address", peer_address);
                error!(?e);
            }
        }

        Ok(())
    }

    async fn send_scroll(
        peer_address: &str,
        source: ScrollSource,
        delta_x: f64,
        delta_y: f64,
    ) -> Result<()> {
        let proto_source = match source {
            ScrollSource::Wheel => ProtoScrollSource::Wheel,
            ScrollSource::Finger => ProtoScrollSource::Finger,
            ScrollSource::Continuous => ProtoScrollSource::Continuous,
        };

        let event = ScrollEvent {
            source: proto_source.into(),
            delta_x,
            delta_y,
        };

        let mut client = DaemonSender::connect(peer_address).await?;

        let (tx, rx) = mpsc::channel(1);
        tx.send(event).await
            .map_err(|e| Error::wrap(e, ErrorKind::Network)
                .with_msg("daemon: Failed to queue scroll event"))?;
        drop(tx);

        let out_stream = ReceiverStream::new(rx);

        match client.scroll(out_stream).await {
            Ok(response) => {
                let mut in_stream = response.into_inner();
                while let Some(result) = in_stream.next().await {
                    if let Err(e) = result {
                        let e = Error::wrap(e, ErrorKind::Network)
                            .with_msg("daemon: Scroll stream error");
                        error!(?e);
                        break;
                    }
                }
            }
            Err(e) => {
                let e = Error::wrap(e, ErrorKind::Network)
                    .with_msg("daemon: Failed to send scroll")
                    .with_ctx("address", peer_address);
                error!(?e);
            }
        }

        Ok(())
    }

    async fn run_clipboard(
        config: Config,
        key_store: Arc<KeyStore>,
        last_set: Arc<AtomicU64>,
        cancel: CancellationToken,
    ) -> Result<()> {
        info!("Starting clipboard source");

        let mut clipboard_rx = clipboard::watch_clipboard().await?;

        loop {
            tokio::select! {
                _ = cancel.cancelled() => break,
                change = clipboard_rx.recv() => {
                    if change.is_none() {
                        break;
                    }
                }
            };

            let last_set = last_set.load(Ordering::SeqCst);
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
                    if let Err(e) = DaemonSender::send_clipboard(
                        &config,
                        &key_store,
                        &peer.address,
                        &peer.public_key,
                        &clipboard_text,
                    ).await {
                        let e = Error::wrap(e, ErrorKind::Network)
                            .with_msg("daemon: Failed to send clipboard to peer")
                            .with_ctx("address", &peer.address);
                        error!(?e);
                    }
                }
            }
        }

        Ok(())
    }

    fn run_scroll_stream(
        device_path: String,
        tx: mpsc::Sender<scroll::ScrollEvent>,
        cancel: CancellationToken,
    ) -> Result<()> {
        let mut receiver = ScrollReceiver::new(&device_path)?;
        info!("Started scroll receiver on {}", device_path);

        while !cancel.is_cancelled() {
            match receiver.read_event() {
                Ok(Some(event)) => {
                    trace!(
                        delta_x = event.delta_x,
                        delta_y = event.delta_y,
                        "Scroll event",
                    );
                    if tx.blocking_send(event).is_err() {
                        break;
                    }
                }
                Ok(None) => {}
                Err(e) => {
                    let e = Error::wrap(e, ErrorKind::Read)
                        .with_msg("daemon: Scroll receiver error");
                    error!(?e);
                    return Err(e);
                }
            }
        }

        Ok(())
    }

    async fn run_scroll(config: Config, cancel: CancellationToken) -> Result<()> {
        info!("Starting scroll source");

        let (tx, mut rx) = tokio::sync::mpsc::channel(32);

        for device_path in config.server.scroll_input_devices.clone() {
            let tx = tx.clone();
            let receiver_cancel = cancel.clone();
            tokio::task::spawn_blocking(move || {
                if let Err(e) = DaemonSender::run_scroll_stream(
                    device_path,
                    tx,
                    receiver_cancel,
                ) {
                    let e = Error::wrap(e, ErrorKind::Exec)
                        .with_msg("daemon: Scroll receiver thread failed");
                    error!(?e);
                }
            });
        }

        loop {
            let event = tokio::select! {
                _ = cancel.cancelled() => break,
                result = rx.recv() => {
                    match result {
                        Some(event) => event,
                        None => break,
                    }
                }
            };

            let (delta_x, delta_y) = if config.server.scroll_reverse {
                (-event.delta_x, -event.delta_y)
            } else {
                (event.delta_x, event.delta_y)
            };

            trace!(
                source = ?event.source,
                delta_x = delta_x,
                delta_y = delta_y,
                "Sending scroll event"
            );

            for peer in &config.peers {
                if peer.scroll_destination {
                    if let Err(e) = DaemonSender::send_scroll(
                        &peer.address,
                        event.source,
                        delta_x,
                        delta_y,
                    ).await {
                        let e = Error::wrap(e, ErrorKind::Network)
                            .with_msg("daemon: Failed to send scroll to peer")
                            .with_ctx("address", &peer.address);
                        error!(?e);
                    }
                }
            }
        }

        Ok(())
    }

    pub(crate) async fn run(config: Config) -> Result<()> {
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

        let last_set_clipboard = Arc::new(AtomicU64::new(0));
        let last_active = Arc::new(AtomicU64::new(0));
        let cancel = CancellationToken::new();
        let mut handles = Vec::new();

        if should_run_server {
            let server_config = config.clone();
            let server_key_store = key_store.clone();
            let last_set = last_set_clipboard.clone();
            let last_scr = last_active.clone();
            handles.push(tokio::spawn(async move {
                if let Err(e) = DaemonReceiver::run(
                    server_config,
                    server_key_store,
                    last_set,
                    last_scr,
                ).await {
                    let e = Error::wrap(e, ErrorKind::Network)
                        .with_msg("daemon: Server error");
                    error!(?e);
                }
            }));
        }

        if should_run_clipboard_source {
            let clipboard_config = config.clone();
            let clipboard_key_store = key_store.clone();
            let last_set = last_set_clipboard.clone();
            let clipboard_cancel = cancel.clone();
            handles.push(tokio::spawn(async move {
                if let Err(e) = DaemonSender::run_clipboard(
                    clipboard_config,
                    clipboard_key_store,
                    last_set,
                    clipboard_cancel,
                ).await {
                    let e = Error::wrap(e, ErrorKind::Exec)
                        .with_msg("daemon: Clipboard source error");
                    error!(?e);
                }
            }));
        }

        if should_run_scroll_source {
            let scroll_config = config.clone();
            let scroll_cancel = cancel.clone();
            handles.push(tokio::spawn(async move {
                if let Err(e) = DaemonSender::run_scroll(
                    scroll_config,
                    scroll_cancel,
                ).await {
                    let e = Error::wrap(e, ErrorKind::Exec)
                        .with_msg("daemon: Scroll source error");
                    error!(?e);
                }
            }));
        }

        if config.server.scroll_destination {
            for device_path in config.server.scroll_input_devices.clone() {
                let last_scr = last_active.clone();
                let blocker_cancel = cancel.clone();
                let blocker_fd: Arc<AtomicI32> = Arc::new(AtomicI32::new(-1));
                let blocker_fd_ref = blocker_fd.clone();

                tokio::task::spawn_blocking(move || {
                    let mut blocker = match ScrollBlocker::new(&device_path, last_scr) {
                        Ok(b) => b,
                        Err(e) => {
                            let e = Error::wrap(e, ErrorKind::Exec)
                                .with_msg("daemon: Failed to start scroll blocker");
                            error!(?e);
                            return;
                        }
                    };
                    info!("Started scroll blocker on {}", device_path);
                    blocker_fd_ref.store(blocker.device_fd(), Ordering::SeqCst);
                    if let Err(e) = blocker.run(blocker_cancel) {
                        let e = Error::wrap(e, ErrorKind::Exec)
                            .with_msg("daemon: Scroll blocker error");
                        error!(?e);
                    }
                });

                let close_cancel = cancel.clone();
                tokio::spawn(async move {
                    close_cancel.cancelled().await;
                    let fd = blocker_fd.load(Ordering::SeqCst);
                    if fd >= 0 {
                        ScrollBlocker::release(fd);
                    }
                });
            }
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

        cancel.cancel();

        for handle in handles {
            handle.abort();
        }

        sleep(Duration::from_millis(500)).await;
        std::process::exit(0)
    }
}
