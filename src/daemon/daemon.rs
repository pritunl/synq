use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration};
use tracing::{error, info, warn, trace};
use tokio::sync::mpsc;
use tokio_stream::wrappers::ReceiverStream;
use tokio_stream::StreamExt;
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
use crate::utils;
use crate::synq::{
    synq_service_server::{SynqService, SynqServiceServer},
    synq_service_client::SynqServiceClient,
    ScrollEvent, ClipboardEvent,
};

const CLIPBOARD_TTL: u64 = 500;

pub struct DaemonServer {
    config: Config,
    last_set_clipboard: Arc<AtomicU64>,
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

async fn run_server(config: Config, last_set_clipboard: Arc<AtomicU64>) -> Result<()> {
    let addr = config.server.bind.parse()
        .map_err(|e| Error::wrap(e, ErrorKind::Parse)
            .with_msg("daemon: Failed to parse bind address"))?;

    let server = DaemonServer { config, last_set_clipboard };

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

async fn send_scroll_to_peer(peer_address: &str, delta_x: i32, delta_y: i32) -> Result<()> {
    let event = ScrollEvent { delta_x, delta_y };

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

async fn run_clipboard_source(config: Config, last_set: Arc<AtomicU64>) -> Result<()> {
    info!("Starting clipboard source");

    let mut clipboard_rx = clipboard::watch_clipboard().await?;

    while let Some(_change) = clipboard_rx.recv().await {
        let last_set = last_set.load(Ordering::SeqCst);
        if utils::mono_time_ms().saturating_sub(last_set) < CLIPBOARD_TTL {
            trace!("Ignoring clipboard change within debounce window");
            continue;
        }

        let clipboard_text = match clipboard::get_clipboard().await {
            Ok(text) => text,
            Err(e) => {
                warn!("Failed to get clipboard: {:?}", e);
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
                    error!("Failed to send clipboard to {}: {:?}", peer.address, e);
                }
            }
        }
    }

    Ok(())
}

async fn run_scroll_source(config: Config) -> Result<()> {
    info!("Starting scroll source");

    let mut interval = tokio::time::interval(Duration::from_secs(10));

    loop {
        interval.tick().await;

        let delta_x = 5;
        let delta_y = 5;

        trace!("Sending scroll event: delta_x={} delta_y={}", delta_x, delta_y);

        for peer in &config.peers {
            if peer.scroll_destination {
                if let Err(e) = send_scroll_to_peer(&peer.address, delta_x, delta_y).await {
                    error!("Failed to send scroll to {}: {:?}", peer.address, e);
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

    let last_set_clipboard = Arc::new(AtomicU64::new(0));
    let mut handles = Vec::new();

    if should_run_server {
        let server_config = config.clone();
        let last_set = last_set_clipboard.clone();
        handles.push(tokio::spawn(async move {
            if let Err(e) = run_server(server_config, last_set).await {
                error!("Server error: {:?}", e);
            }
        }));
    }

    if should_run_clipboard_source {
        let clipboard_config = config.clone();
        let last_set = last_set_clipboard.clone();
        handles.push(tokio::spawn(async move {
            if let Err(e) = run_clipboard_source(clipboard_config, last_set).await {
                error!("Clipboard source error: {:?}", e);
            }
        }));
    }

    if should_run_scroll_source {
        let scroll_config = config.clone();
        handles.push(tokio::spawn(async move {
            if let Err(e) = run_scroll_source(scroll_config).await {
                error!("Scroll source error: {:?}", e);
            }
        }));
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

    // TODO
    for handle in handles {
        handle.abort();
    }

    std::process::exit(0)
}
