use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU64, Ordering};

use tokio::sync::{mpsc, RwLock};
use tokio_util::sync::CancellationToken;

use crate::errors::{error, info, warn};
use crate::errors::{Result, Error, ErrorKind};
use crate::config::Config;
use crate::crypto::KeyStore;
use crate::synq::ScrollEvent;

use super::server::TransportServer;
use super::scroll::ScrollTransport;
use super::clipboard::{ClipboardTransport, ClipboardSendEvent};
use super::active::{ActiveState, ActiveTransport, ActiveRequestEvent};

const SCROLL_INJECT_CAPACITY: usize = 32;

#[derive(Debug, Clone)]
#[allow(dead_code)]
pub enum PeerState {
    Connected,
    Connecting,
    Disconnected,
}

#[allow(dead_code)]
pub struct TransportStatus {
    peers: RwLock<Vec<(String, Arc<AtomicU8>)>>,
    server_running: AtomicBool,
}

impl TransportStatus {
    fn new() -> Self {
        Self {
            peers: RwLock::new(Vec::new()),
            server_running: AtomicBool::new(false),
        }
    }
}

#[derive(Clone)]
pub struct Transport {
    scroll_tx: mpsc::Sender<ScrollEvent>,
    clipboard_tx: mpsc::Sender<ClipboardSendEvent>,
    active_tx: mpsc::Sender<ActiveRequestEvent>,
    active_state: ActiveState,
    #[allow(dead_code)]
    status: Arc<TransportStatus>,
    last_set_clipboard: Arc<AtomicU64>,
    cancel: CancellationToken,
}

pub struct ScrollInjectRx {
    rx: std::sync::mpsc::Receiver<ScrollEvent>,
}

impl ScrollInjectRx {
    pub fn recv(&self) -> Option<ScrollEvent> {
        self.rx.recv().ok()
    }
}

impl Transport {
    pub async fn new(
        config: &Config,
        key_store: Arc<KeyStore>,
    ) -> Result<(Self, Option<ScrollInjectRx>)> {
        let cancel = CancellationToken::new();
        let status = Arc::new(TransportStatus::new());
        let last_set_clipboard = Arc::new(AtomicU64::new(0));

        let (scroll_inject_rx, scroll_inject_tx) = if config.server.scroll_destination {
            let (tx, rx) = std::sync::mpsc::sync_channel(SCROLL_INJECT_CAPACITY);
            (Some(ScrollInjectRx { rx }), Some(tx))
        } else {
            (None, None)
        };

        let active_state = ActiveState::new(config.server.public_key.clone());

        let should_run_server = config.server.clipboard_destination
            || config.server.scroll_destination
            || config.server.scroll_source;
        if should_run_server {
            let server = TransportServer::new(
                config.clone(),
                key_store.clone(),
                last_set_clipboard.clone(),
                scroll_inject_tx,
                active_state.clone(),
            );
            let server_status = status.clone();
            tokio::spawn(async move {
                server_status.server_running.store(true, Ordering::Relaxed);
                if let Err(e) = server.run().await {
                    let e = Error::wrap(e, ErrorKind::Network)
                        .with_msg("transport: Server error");
                    error(&e);
                }
                server_status.server_running.store(false, Ordering::Relaxed);
            });
        }

        let (scroll_tx, peer_states) = ScrollTransport::start(
            &config.peers,
            active_state.clone(),
            cancel.clone(),
        );
        {
            let mut peers = status.peers.write().await;
            *peers = peer_states;
        }

        let clipboard_tx = ClipboardTransport::start(
            key_store.clone(),
            config.server.public_key.clone(),
        );

        let active_tx = ActiveTransport::start(
            &config.peers,
            config.server.public_key.clone(),
            active_state.clone(),
            cancel.clone(),
        );

        info!("Transport initialized");

        Ok((
            Self {
                scroll_tx,
                clipboard_tx,
                active_tx,
                active_state,
                status,
                last_set_clipboard,
                cancel,
            },
            scroll_inject_rx,
        ))
    }

    pub fn send_scroll(&self, event: ScrollEvent) -> bool {
        if let Err(e) = self.scroll_tx.try_send(event) {
            warn!("transport: Dropped scroll event: {}", e);
            return false;
        }
        true
    }

    pub fn send_clipboard(
        &self, peer_address: String,
        peer_public_key: String,
        text: String,
    ) -> bool {
        if let Err(e) = self.clipboard_tx.try_send(ClipboardSendEvent {
            peer_address,
            peer_public_key,
            text,
        }) {
            warn!("transport: Dropped clipboard send event: {}", e);
            return false;
        }
        true
    }

    #[allow(dead_code)]
    pub async fn status(&self) -> Vec<(String, PeerState)> {
        let peers = self.status.peers.read().await;
        peers.iter()
            .map(|(addr, state)| (addr.clone(), ScrollTransport::atomic_to_peer_state(state)))
            .collect()
    }

    #[allow(dead_code)]
    pub fn server_running(&self) -> bool {
        self.status.server_running.load(Ordering::Relaxed)
    }

    pub fn last_set_clipboard(&self) -> &AtomicU64 {
        &self.last_set_clipboard
    }

    pub fn send_active_request(&self) -> bool {
        if let Err(e) = self.active_tx.try_send(ActiveRequestEvent) {
            warn!("transport: Dropped active request event: {}", e);
            return false;
        }
        true
    }

    pub fn active_state(&self) -> &ActiveState {
        &self.active_state
    }

    pub fn shutdown(&self) {
        self.cancel.cancel();
    }

    pub fn cancel_token(&self) -> CancellationToken {
        self.cancel.clone()
    }
}
