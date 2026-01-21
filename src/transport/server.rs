use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use tokio_stream::StreamExt;
use tokio::sync::mpsc;
use tonic::{
    transport::Server as TonicServer,
    Request,
    Response,
    Status,
    Streaming,
};

use crate::errors::{error, warn, trace};
use crate::errors::{Result, Error, ErrorKind};
use crate::config::Config;
use crate::crypto;
use crate::crypto::KeyStore;
use crate::clipboard;
use crate::utils;
use crate::synq::{
    synq_service_server::{SynqService, SynqServiceServer},
    ScrollEvent, ClipboardEvent, ActiveEvent, ActivateEvent, Empty,
};

use super::active::{ActiveState, ActiveRequestEvent, send_active_state};
use super::constants::SCROLL_TTL;

pub struct TransportServer {
    config: Config,
    key_store: Arc<KeyStore>,
    last_set_clipboard: Arc<AtomicU64>,
    scroll_inject_tx: Option<std::sync::mpsc::SyncSender<ScrollEvent>>,
    active_tx: mpsc::Sender<ActiveRequestEvent>,
    active_state: ActiveState,
}

#[tonic::async_trait]
impl SynqService for TransportServer {
    async fn scroll(
        &self,
        request: Request<Streaming<ScrollEvent>>,
    ) -> std::result::Result<Response<Empty>, Status> {
        if !self.config.server.scroll_destination {
            return Err(Status::permission_denied("scroll destination not enabled"));
        }

        let scroll_tx = match &self.scroll_inject_tx {
            Some(tx) => tx.clone(),
            None => return Err(Status::unavailable("scroll sender not initialized")),
        };

        trace!("Scroll connection established");

        let mut in_stream = request.into_inner();
        while let Some(result) = in_stream.next().await {
            match result {
                Ok(evt) => {
                    trace!(
                        delta_x = evt.delta_x,
                        delta_y = evt.delta_y,
                        "Received scroll event",
                    );

                    let last_scroll = self.active_state.get_last_scroll();
                    let now = utils::mono_time_ms();

                    if last_scroll > 0 && now - last_scroll > SCROLL_TTL {
                        if self.active_state.is_host_active() {
                            self.send_deactivate_request();
                        }
                        continue;
                    }

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

        Ok(Response::new(Empty {}))
    }

    async fn clipboard(
        &self,
        request: Request<ClipboardEvent>,
    ) -> std::result::Result<Response<Empty>, Status> {
        if !self.config.server.clipboard_destination {
            return Err(Status::permission_denied("clipboard destination not enabled"));
        }

        let event = request.into_inner();

        if let Err(e) = handle_clipboard_event(
            &self.config, &self.key_store, &self.last_set_clipboard, event,
        ).await {
            error(&e);
            return Err(Status::internal("failed to handle clipboard event"));
        }

        Ok(Response::new(Empty {}))
    }

    async fn activate_request(
        &self,
        request: Request<ActivateEvent>,
    ) -> std::result::Result<Response<ActiveEvent>, Status> {
        if !self.config.server.scroll_source {
            return Err(Status::permission_denied("scroll source not enabled"));
        }

        let event = request.into_inner();

        let peer = self.config.peers.iter()
            .find(|p| p.public_key == event.peer);

        let peer = match peer {
            Some(p) => p,
            None => {
                warn!("Received activate request from unknown peer: {}", event.peer);
                return Err(Status::permission_denied("unknown peer"));
            }
        };

        if !peer.scroll_destination {
            warn!("Received activate request from non-destination peer: {}", event.peer);
            return Err(Status::permission_denied("peer is not a scroll destination"));
        }

        let new_peer = if event.state {
            peer.public_key.clone()
        } else {
            self.config.server.public_key.clone()
        };

        let new_clock = self.active_state.increment_and_set(new_peer.clone());

        trace!(
            peer = %new_peer,
            clock = new_clock,
            state = event.state,
            "Active peer updated via request",
        );

        for dest_peer in &self.config.peers {
            if dest_peer.scroll_destination && dest_peer.public_key != new_peer {
                let address = dest_peer.address.clone();
                let peer_key = new_peer.clone();
                let clock = new_clock;
                tokio::spawn(async move {
                    if let Err(e) = send_active_state(&address, &peer_key, clock).await {
                        error(&e);
                    }
                });
            }
        }

        Ok(Response::new(ActiveEvent {
            peer: new_peer,
            clock: new_clock,
        }))
    }

    async fn active_state(
        &self,
        request: Request<ActiveEvent>,
    ) -> std::result::Result<Response<Empty>, Status> {
        let event = request.into_inner();

        if event.clock == 0 {
            self.active_state.reset();
            trace!(
                peer = %event.peer,
                "Active state reset",
            );
            return Ok(Response::new(Empty {}));
        }

        let current_clock = self.active_state.get_clock();
        if event.clock <= current_clock {
            trace!(
                peer = %event.peer,
                event_clock = event.clock,
                current_clock = current_clock,
                "Ignoring stale active state",
            );
            return Ok(Response::new(Empty {}));
        }

        self.active_state.set_active(event.peer.clone(), event.clock);

        trace!(
            peer = %event.peer,
            clock = event.clock,
            "Active state updated",
        );

        Ok(Response::new(Empty {}))
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
    clipboard::set_clipboard(plaintext);

    Ok(())
}

impl TransportServer {
    pub fn new(
        config: Config,
        key_store: Arc<KeyStore>,
        last_set_clipboard: Arc<AtomicU64>,
        scroll_inject_tx: Option<std::sync::mpsc::SyncSender<ScrollEvent>>,
        active_tx: mpsc::Sender<ActiveRequestEvent>,
        active_state: ActiveState,
    ) -> Self {
        Self {
            config,
            key_store,
            last_set_clipboard,
            scroll_inject_tx,
            active_tx,
            active_state,
        }
    }

    pub fn send_deactivate_request(&self) -> bool {
        if let Err(e) = self.active_tx.try_send(ActiveRequestEvent::Deactivate) {
            warn!("transport: Dropped deactivate request event: {}", e);
            return false;
        }
        true
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
