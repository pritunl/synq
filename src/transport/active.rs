use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::RwLock;

use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use tonic::transport::Channel;

use crate::errors::{error, trace};
use crate::errors::{Error, ErrorKind};
use crate::config::PeerConfig;
use crate::synq::{
    synq_service_client::SynqServiceClient,
    ActiveEvent,
};


#[derive(Clone)]
pub struct ActiveState {
    active_peer: Arc<RwLock<Option<String>>>,
    clock: Arc<AtomicU64>,
    host_active: Arc<AtomicBool>,
    host_public_key: Arc<String>,
}

impl ActiveState {
    pub fn new(host_public_key: String) -> Self {
        Self {
            active_peer: Arc::new(RwLock::new(None)),
            clock: Arc::new(AtomicU64::new(0)),
            host_active: Arc::new(AtomicBool::new(false)),
            host_public_key: Arc::new(host_public_key),
        }
    }

    pub fn is_host_active(&self) -> bool {
        self.host_active.load(Ordering::Acquire)
    }

    pub fn get_active_peer(&self) -> Option<String> {
        self.active_peer.read().unwrap().clone()
    }

    pub fn get_clock(&self) -> u64 {
        self.clock.load(Ordering::SeqCst)
    }

    pub fn set_active(&self, peer: String, clock: u64) {
        let is_host = peer == *self.host_public_key;
        *self.active_peer.write().unwrap() = Some(peer);
        self.clock.store(clock, Ordering::SeqCst);
        self.host_active.store(is_host, Ordering::Release);
    }

    pub fn increment_and_set(&self, peer: String) -> u64 {
        let is_host = peer == *self.host_public_key;
        let new_clock = self.clock.fetch_add(1, Ordering::SeqCst) + 1;
        *self.active_peer.write().unwrap() = Some(peer);
        self.host_active.store(is_host, Ordering::Release);
        new_clock
    }

    #[allow(dead_code)]
    pub fn is_active(&self, peer: &str, clock: u64) -> bool {
        let current_clock = self.clock.load(Ordering::SeqCst);
        if clock < current_clock {
            return false;
        }
        self.active_peer.read().unwrap().as_deref() == Some(peer)
    }

    pub fn reset(&self) {
        *self.active_peer.write().unwrap() = None;
        self.clock.store(0, Ordering::SeqCst);
        self.host_active.store(false, Ordering::Release);
    }
}

pub struct ActiveRequestEvent;

pub struct ActiveTransport;

impl ActiveTransport {
    pub fn start(
        peers: &[PeerConfig],
        host_public_key: String,
        active_state: ActiveState,
        cancel: CancellationToken,
    ) -> mpsc::Sender<ActiveRequestEvent> {
        let destination_peers: Vec<_> = peers.iter()
            .filter(|p| p.scroll_destination)
            .cloned()
            .collect();

        let source_peer = peers.iter()
            .find(|p| p.scroll_source)
            .cloned();

        let (tx, rx) = mpsc::channel::<ActiveRequestEvent>(32);

        tokio::spawn(async move {
            run_active_handler(
                rx,
                source_peer,
                destination_peers,
                host_public_key,
                active_state,
                cancel,
            ).await;
        });

        tx
    }
}

async fn run_active_handler(
    mut rx: mpsc::Receiver<ActiveRequestEvent>,
    source_peer: Option<PeerConfig>,
    _destination_peers: Vec<PeerConfig>,
    host_public_key: String,
    active_state: ActiveState,
    cancel: CancellationToken,
) {
    loop {
        let _event = tokio::select! {
            _ = cancel.cancelled() => break,
            result = rx.recv() => {
                match result {
                    Some(e) => e,
                    None => break,
                }
            }
        };

        if let Some(active_peer) = active_state.get_active_peer() {
            if active_peer == host_public_key {
                trace!(
                    peer = %host_public_key,
                    active_peer = active_peer,
                    "Already active, skipping active request",
                );
                continue;
            }
        }

        let Some(ref source) = source_peer else {
            trace!("No scroll source peer configured, cannot send active request");
            continue;
        };

        trace!(
            address = &source.address,
            peer = %host_public_key,
            "Sending active request to source",
        );

        match send_active_request(&source.address, &host_public_key).await {
            Ok(response) => {
                active_state.set_active(response.peer.clone(), response.clock);
                trace!(
                    peer = %response.peer,
                    clock = response.clock,
                    "Received active response",
                );
            }
            Err(e) => {
                error(&e);
            }
        }
    }
}

async fn send_active_request(
    address: &str,
    host_public_key: &str,
) -> crate::errors::Result<ActiveEvent> {
    let mut client = connect(address).await?;

    let request = ActiveEvent {
        peer: host_public_key.to_string(),
        clock: 0,
    };

    let response = client.active_request(request)
        .await
        .map_err(|e| Error::wrap(e, ErrorKind::Network)
            .with_msg("transport: Active request failed")
            .with_ctx("address", address))?;

    Ok(response.into_inner())
}

pub async fn send_active_state(
    address: &str,
    peer: &str,
    clock: u64,
) -> crate::errors::Result<()> {
    let mut client = connect(address).await?;

    let event = ActiveEvent {
        peer: peer.to_string(),
        clock,
    };

    client.active_state(event)
        .await
        .map_err(|e| Error::wrap(e, ErrorKind::Network)
            .with_msg("transport: Active state send failed")
            .with_ctx("address", address))?;

    Ok(())
}

async fn connect(address: &str) -> crate::errors::Result<SynqServiceClient<Channel>> {
    let host_port = match address.find('@') {
        Some(i) => &address[i + 1..],
        None => address,
    };

    let url = format!("http://{}", host_port);

    let channel = Channel::from_shared(url)
        .map_err(|e| Error::wrap(e, ErrorKind::Network)
            .with_msg("transport: Invalid peer address")
            .with_ctx("address", address))?
        .connect()
        .await
        .map_err(|e| Error::wrap(e, ErrorKind::Network)
            .with_msg("transport: Failed to connect to peer")
            .with_ctx("address", address))?;

    Ok(SynqServiceClient::new(channel))
}
