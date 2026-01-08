use std::sync::Arc;
use std::sync::atomic::{AtomicI32, AtomicU64, Ordering};
use std::sync::mpsc::{Sender, Receiver};
use std::sync::Mutex;
use tracing::{error, info, warn, trace};
use tokio::sync::mpsc;
use tokio::time::{sleep, Duration};
use tokio_stream::wrappers::ReceiverStream;
use tokio_stream::StreamExt;
use tokio_util::sync::CancellationToken;
use tonic::{
    transport::Server as TonicServer,
    transport::Channel,
    Request,
    Response,
    Status,
    Streaming,
};
use futures::stream;

use crate::errors::{Result, Error, ErrorKind};
use crate::config::Config;
use crate::crypto;
use crate::clipboard;
use crate::scroll;
use crate::scroll::{ScrollSender, ScrollReceiver, ScrollBlocker, ScrollSource};
use crate::utils;
use crate::synq::{
    synq_service_server::{SynqService, SynqServiceServer},
    synq_service_client::SynqServiceClient,
    ScrollEvent, ClipboardEvent, ScrollSource as ProtoScrollSource,
};

const CLIPBOARD_TTL: u64 = 500;

pub struct DaemonServer {
    config: Config,
    last_set_clipboard: Arc<AtomicU64>,
    last_scroll: Arc<AtomicU64>,
    scroll_tx: Option<Sender<ScrollEvent>>,
}

#[tonic::async_trait]
impl SynqService for DaemonServer {
    type ScrollStream = stream::Empty<Result<ScrollEvent, Status>>;
    type ClipboardStream = stream::Empty<Result<ClipboardEvent, Status>>;

    async fn scroll(
        &self,
        request: Request<Streaming<ScrollEvent>>,
    ) -> Result<Response<Self::ScrollStream>, Status> {
        if !self.config.server.scroll_destination {
            return Err(Status::permission_denied("scroll destination not enabled"));
        }

        let scroll_tx = match &self.scroll_tx {
            Some(tx) => tx.clone(),
            None => return Err(Status::unavailable("scroll sender not initialized")),
        };

        trace!("Scroll connection established");

        let mut in_stream = request.into_inner();

        tokio::spawn(async move {
            while let Some(result) = in_stream.next().await {
                match result {
                    Ok(evt) => {
                        trace!(
                            "Scroll event: delta_x={} delta_y={}",
                            evt.delta_x, evt.delta_y,
                        );
                        if scroll_tx.send(evt).is_err() {
                            error!("Scroll sender channel closed");
                            break;
                        }
                    }
                    Err(e) => {
                        let e = Error::wrap(e, ErrorKind::Network)
                            .with_msg("daemon: Failed to read scroll event");
                        error!(?e);
                        break;
                    }
                }
            }
            trace!("Scroll connection closed");
        });

        Ok(Response::new(stream::empty()))
    }

    async fn clipboard(
        &self,
        request: Request<Streaming<ClipboardEvent>>,
    ) -> Result<Response<Self::ClipboardStream>, Status> {
        if !self.config.server.clipboard_destination {
            return Err(Status::permission_denied("clipboard destination not enabled"));
        }

        trace!("Clipboard connection established");

        let config = self.config.clone();
        let last_set_clipboard = self.last_set_clipboard.clone();
        let mut in_stream = request.into_inner();

        tokio::spawn(async move {
            while let Some(result) = in_stream.next().await {
                match result {
                    Ok(event) => {
                        if let Err(e) = handle_clipboard_event(
                            &config, &last_set_clipboard, event,
                        ).await {
                            error!(?e);
                        }
                    }
                    Err(e) => {
                        let e = Error::wrap(e, ErrorKind::Network)
                            .with_msg("daemon: Failed to read clipboard event");
                        error!(?e);
                        break;
                    }
                }
            }
            trace!("Clipboard connection closed");
        });

        Ok(Response::new(stream::empty()))
    }
}

fn handle_scroll_event(
    scroll_sender: &mut ScrollSender,
    event: ScrollEvent,
) -> Result<()> {
    scroll_sender.send(event.delta_x, event.delta_y)
}

async fn handle_clipboard_event(
    config: &Config,
    last_set_clipboard: &AtomicU64,
    event: ClipboardEvent,
) -> Result<()> {
    let peer = config.peers.iter()
        .find(|p| p.public_key == event.client);

    let peer = match peer {
        Some(p) => p,
        None => {
            warn!("Received clipboard event from unknown: {}", event.client);
            return Ok(());
        }
    };

    if !peer.clipboard_source {
        warn!("Received clipboard event from unauthorized: {}", event.client);
        return Ok(());
    }

    let ciphertext = String::from_utf8(event.data)
        .map_err(|e| Error::wrap(e, ErrorKind::Parse)
            .with_msg("daemon: Invalid UTF-8 in clipboard ciphertext"))?;

    let plaintext = crypto::decrypt(
        &config.server.private_key,
        &peer.public_key,
        &ciphertext,
    )?;

    trace!("Received clipboard from peer {}", peer.address);

    last_set_clipboard.store(utils::mono_time_ms(), Ordering::SeqCst);
    clipboard::set_clipboard(plaintext).await?;

    Ok(())
}

fn run_scroll_sender(rx: Receiver<ScrollEvent>) {
    let mut sender = match ScrollSender::new() {
        Ok(s) => s,
        Err(e) => {
            let e = Error::wrap(e, ErrorKind::Exec)
                .with_msg("daemon: Failed to create scroll sender");
            error!(?e);
            return;
        }
    };
    info!("Started scroll sender");

    while let Ok(event) = rx.recv() {
        if let Err(e) = handle_scroll_event(&mut sender, event) {
            let e = Error::wrap(e, ErrorKind::Exec)
                .with_msg("daemon: Failed to send scroll event");
            error!(?e);
        }
    }
}

async fn run_server(
    config: Config,
    last_set_clipboard: Arc<AtomicU64>,
    last_scroll: Arc<AtomicU64>,
) -> Result<()> {
    let addr = config.server.bind.parse()
        .map_err(|e| Error::wrap(e, ErrorKind::Parse)
            .with_msg("daemon: Failed to parse bind address"))?;

    let scroll_tx = if config.server.scroll_destination {
        let (tx, rx) = std::sync::mpsc::channel();
        tokio::task::spawn_blocking(move || run_scroll_sender(rx));
        Some(tx)
    } else {
        None
    };

    let server = DaemonServer { config, last_set_clipboard, last_scroll, scroll_tx };

    info!("Daemon server listening on {}", addr);

    TonicServer::builder()
        .add_service(SynqServiceServer::new(server))
        .serve(addr)
        .await
        .map_err(|e| Error::wrap(e, ErrorKind::Network)
            .with_msg("daemon: Server failed"))?;

    Ok(())
}

async fn connect_to_peer(address: &str) -> Result<SynqServiceClient<Channel>> {
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

async fn send_clipboard_to_peer(
    config: &Config,
    peer_address: &str,
    peer_public_key: &str,
    clipboard_text: &str,
) -> Result<()> {
    let encrypted = crypto::encrypt(
        &config.server.private_key,
        peer_public_key,
        clipboard_text,
    )?;

    let event = ClipboardEvent {
        client: config.server.public_key.clone(),
        data: encrypted.into_bytes(),
    };

    let mut client = connect_to_peer(peer_address).await?;

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

async fn send_scroll_to_peer(
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

    let mut client = connect_to_peer(peer_address).await?;

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

async fn run_clipboard_source(
    config: Config,
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
                if let Err(e) = send_clipboard_to_peer(
                    &config,
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

fn run_scroll_receiver(
    device_path: String,
    tx: Sender<scroll::ScrollEvent>,
    cancel: CancellationToken,
) -> Result<()> {
    let mut receiver = ScrollReceiver::new(&device_path)?;
    info!("Started scroll receiver on {}", device_path);

    while !cancel.is_cancelled() {
        match receiver.read_event() {
            Ok(Some(event)) => {
                trace!(delta_x = event.delta_x, delta_y = event.delta_y, "Scroll event");
                if tx.send(event).is_err() {
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

async fn run_scroll_source(config: Config, cancel: CancellationToken) -> Result<()> {
    info!("Starting scroll source");

    let device_path = config.server.scroll_input_device.clone();
    let (tx, rx) = std::sync::mpsc::channel();
    let rx = Arc::new(Mutex::new(rx));

    let receiver_cancel = cancel.clone();
    tokio::task::spawn_blocking(move || {
        if let Err(e) = run_scroll_receiver(device_path, tx, receiver_cancel) {
            let e = Error::wrap(e, ErrorKind::Exec)
                .with_msg("daemon: Scroll receiver thread failed");
            error!(?e);
        }
    });

    loop {
        let event = tokio::select! {
            _ = cancel.cancelled() => break,
            result = tokio::task::spawn_blocking({
                let rx = rx.clone();
                move || rx.lock().unwrap().recv()
            }) => {
                match result {
                    Ok(Ok(event)) => event,
                    Ok(Err(_)) => break,
                    Err(e) => {
                        let e = Error::wrap(e, ErrorKind::Exec)
                            .with_msg("daemon: Scroll receiver task failed");
                        return Err(e);
                    }
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
                if let Err(e) = send_scroll_to_peer(
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

    let last_set_clipboard = Arc::new(AtomicU64::new(0));
    let last_scroll = Arc::new(AtomicU64::new(0));
    let cancel = CancellationToken::new();
    let mut handles = Vec::new();

    if should_run_server {
        let server_config = config.clone();
        let last_set = last_set_clipboard.clone();
        let last_scr = last_scroll.clone();
        handles.push(tokio::spawn(async move {
            if let Err(e) = run_server(server_config, last_set, last_scr).await {
                let e = Error::wrap(e, ErrorKind::Network)
                    .with_msg("daemon: Server error");
                error!(?e);
            }
        }));
    }

    if should_run_clipboard_source {
        let clipboard_config = config.clone();
        let last_set = last_set_clipboard.clone();
        let clipboard_cancel = cancel.clone();
        handles.push(tokio::spawn(async move {
            if let Err(e) = run_clipboard_source(clipboard_config, last_set, clipboard_cancel).await {
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
            if let Err(e) = run_scroll_source(scroll_config, scroll_cancel).await {
                let e = Error::wrap(e, ErrorKind::Exec)
                    .with_msg("daemon: Scroll source error");
                error!(?e);
            }
        }));
    }

    if config.server.scroll_destination {
        let device_path = config.server.scroll_input_device.clone();
        let last_scr = last_scroll.clone();
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
