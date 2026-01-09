use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{Sender, Receiver};

use tracing::{error, info, warn, trace};
use tokio_stream::StreamExt;
use tonic::{
    transport::Server as TonicServer,
    Request,
    Response,
    Status,
    Streaming,
};
use futures::stream;

use crate::errors::{Result, Error, ErrorKind};
use crate::config::Config;
use crate::crypto;
use crate::crypto::KeyStore;
use crate::clipboard;
use crate::scroll::ScrollSender;
use crate::utils;
use crate::synq::{
    synq_service_server::{SynqService, SynqServiceServer},
    ScrollEvent, ClipboardEvent,
};

pub struct DaemonReceiver {
    config: Config,
    key_store: Arc<KeyStore>,
    last_set_clipboard: Arc<AtomicU64>,
    scroll_tx: Option<Sender<ScrollEvent>>,
}

#[tonic::async_trait]
impl SynqService for DaemonReceiver {
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
        let key_store = self.key_store.clone();
        let last_set_clipboard = self.last_set_clipboard.clone();
        let mut in_stream = request.into_inner();

        tokio::spawn(async move {
            while let Some(result) = in_stream.next().await {
                match result {
                    Ok(event) => {
                        if let Err(e) = DaemonReceiver::handle_clipboard(
                            &config, &key_store, &last_set_clipboard, event,
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

impl DaemonReceiver {
    fn handle_scroll(
        scroll_sender: &mut ScrollSender,
        event: ScrollEvent,
    ) -> Result<()> {
        scroll_sender.send(event.delta_x, event.delta_y)
    }

    fn run_scroll(
        rx: Receiver<ScrollEvent>,
        last_active: Arc<AtomicU64>,
    ) {
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
            // TODO
            // let last = last_active.load(Ordering::SeqCst);
            // if utils::mono_time_ms().saturating_sub(last) > 500 {
            //     continue;
            // }
            _ = last_active;

            if let Err(e) = DaemonReceiver::handle_scroll(&mut sender, event) {
                let e = Error::wrap(e, ErrorKind::Exec)
                    .with_msg("daemon: Failed to send scroll event");
                error!(?e);
            }
        }
    }

    async fn handle_clipboard(
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
                .with_msg("daemon: Invalid UTF-8 in clipboard ciphertext"))?;

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

    pub(crate) async fn run(
        config: Config,
        key_store: Arc<KeyStore>,
        last_set_clipboard: Arc<AtomicU64>,
        last_active: Arc<AtomicU64>,
    ) -> Result<()> {
        let addr = config.server.bind.parse()
            .map_err(|e| Error::wrap(e, ErrorKind::Parse)
                .with_msg("daemon: Failed to parse bind address"))?;

        let scroll_tx = if config.server.scroll_destination {
            let (tx, rx) = std::sync::mpsc::channel();
            tokio::task::spawn_blocking({
                let last_active = last_active.clone();
                move || {
                    DaemonReceiver::run_scroll(rx, last_active);
                }
            });
            Some(tx)
        } else {
            None
        };

        let server = DaemonReceiver {
            config,
            key_store,
            last_set_clipboard,
            scroll_tx,
        };

        info!("Daemon server listening on {}", addr);

        TonicServer::builder()
            .add_service(SynqServiceServer::new(server))
            .serve(addr)
            .await
            .map_err(|e| Error::wrap(e, ErrorKind::Network)
                .with_msg("daemon: Server failed"))?;

        Ok(())
    }
}
