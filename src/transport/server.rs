use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use tokio_stream::StreamExt;
use tonic::{
    transport::Server as TonicServer,
    Request,
    Response,
    Status,
    Streaming,
};
use futures::stream;

use crate::errors::{error, warn, trace};
use crate::errors::{Result, Error, ErrorKind};
use crate::config::Config;
use crate::crypto;
use crate::crypto::KeyStore;
use crate::clipboard;
use crate::utils;
use crate::synq::{
    synq_service_server::{SynqService, SynqServiceServer},
    ScrollEvent, ClipboardEvent,
};

pub struct TransportServer {
    config: Config,
    key_store: Arc<KeyStore>,
    last_set_clipboard: Arc<AtomicU64>,
    scroll_inject_tx: Option<std::sync::mpsc::SyncSender<ScrollEvent>>,
}

#[tonic::async_trait]
impl SynqService for TransportServer {
    type ScrollStream = stream::Empty<std::result::Result<ScrollEvent, Status>>;
    type ClipboardStream = stream::Empty<std::result::Result<ClipboardEvent, Status>>;

    async fn scroll(
        &self,
        request: Request<Streaming<ScrollEvent>>,
    ) -> std::result::Result<Response<Self::ScrollStream>, Status> {
        if !self.config.server.scroll_destination {
            return Err(Status::permission_denied("scroll destination not enabled"));
        }

        let scroll_tx = match &self.scroll_inject_tx {
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
                            delta_x = evt.delta_x,
                            delta_y = evt.delta_y,
                            "Received scroll event",
                        );

                        if let Err(std::sync::mpsc::TrySendError::Disconnected(_)) =
                            scroll_tx.try_send(evt)
                        {

                            let e = Error::new(ErrorKind::Network)
                                .with_msg("transport: Scroll inject channel closed");
                            error(&e);
                            break;
                        }
                    }
                    Err(e) => {
                        let e = Error::wrap(e, ErrorKind::Network)
                            .with_msg("transport: Failed to read scroll event");
                        error(&e);
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
    ) -> std::result::Result<Response<Self::ClipboardStream>, Status> {
        if !self.config.server.clipboard_destination {
            return Err(Status::permission_denied("clipboard destination not enabled"));
        }

        trace!("Clipboard connection established");

        let config = self.config.clone();
        let key_store = self.key_store.clone();
        let last_set_clipboard = self.last_set_clipboard.clone();
        let mut in_stream = request.into_inner();

        tokio::spawn(async move {
            while let Some(result) = in_stream.next().await {
                match result {
                    Ok(event) => {
                        if let Err(e) = handle_clipboard_event(
                            &config, &key_store, &last_set_clipboard, event,
                        ).await {
                            error(&e);
                        }
                    }
                    Err(e) => {
                        let e = Error::wrap(e, ErrorKind::Network)
                            .with_msg("transport: Failed to read clipboard event");
                        error(&e);
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
    key_store: &KeyStore,
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
            .with_msg("transport: Invalid UTF-8 in clipboard ciphertext"))?;

    let plaintext = crypto::decrypt(
        key_store,
        &peer.public_key,
        &ciphertext,
    )?;

    trace!("Received clipboard from peer {}", peer.address);

    last_set_clipboard.store(utils::mono_time_ms(), Ordering::SeqCst);
    clipboard::set_clipboard(plaintext).await?;

    Ok(())
}

impl TransportServer {
    pub fn new(
        config: Config,
        key_store: Arc<KeyStore>,
        last_set_clipboard: Arc<AtomicU64>,
        scroll_inject_tx: Option<std::sync::mpsc::SyncSender<ScrollEvent>>,
    ) -> Self {
        Self {
            config,
            key_store,
            last_set_clipboard,
            scroll_inject_tx,
        }
    }

    pub async fn run(self) -> Result<()> {
        let addr = self.config.server.bind.parse()
            .map_err(|e| Error::wrap(e, ErrorKind::Parse)
                .with_msg("transport: Failed to parse bind address"))?;

        crate::errors::info!("Transport server listening on {}", addr);

        TonicServer::builder()
            .add_service(SynqServiceServer::new(self))
            .serve(addr)
            .await
            .map_err(|e| Error::wrap(e, ErrorKind::Network)
                .with_msg("transport: Server failed"))?;

        Ok(())
    }
}
