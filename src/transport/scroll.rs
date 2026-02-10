use std::sync::Arc;
use std::sync::atomic::{AtomicU8, Ordering};

use tokio::sync::mpsc;
use tokio::time::{sleep, Duration};
use tokio_stream::wrappers::ReceiverStream;
use tokio_util::sync::CancellationToken;
use tonic::transport::Channel;

use crate::errors::{error, info, warn};
use crate::errors::{Result, Error, ErrorKind};
use crate::config::PeerConfig;
use crate::synq::{
    synq_service_client::SynqServiceClient,
    ScrollEvent,
};
use super::PeerState;
use super::active::ActiveState;

const CHANNEL_CAPACITY: usize = 32;
const RECONNECT_DELAY_MS: u64 = 1000;

const STATE_DISCONNECTED: u8 = 0;
const STATE_CONNECTING: u8 = 1;
const STATE_CONNECTED: u8 = 2;

pub struct ScrollTransport;

struct PeerInfo {
    public_key: String,
    tx: mpsc::Sender<ScrollEvent>,
}

impl ScrollTransport {
    pub fn start(
        peers: &[PeerConfig],
        active_state: ActiveState,
        cancel: CancellationToken,
    ) -> (mpsc::Sender<ScrollEvent>, Vec<(String, Arc<AtomicU8>)>) {
        let scroll_peers: Vec<_> = peers.iter()
            .filter(|p| p.scroll_destination)
            .collect();

        if scroll_peers.is_empty() {
            let (tx, _rx) = mpsc::channel(1);
            return (tx, Vec::new());
        }

        let (main_tx, mut main_rx) = mpsc::channel::<ScrollEvent>(CHANNEL_CAPACITY);

        let mut peer_infos: Vec<PeerInfo> = Vec::with_capacity(scroll_peers.len());
        let mut peer_states = Vec::with_capacity(scroll_peers.len());

        for peer in scroll_peers {
            let (peer_tx, peer_rx) = mpsc::channel(CHANNEL_CAPACITY);
            let state = Arc::new(AtomicU8::new(STATE_DISCONNECTED));

            peer_states.push((peer.address.clone(), state.clone()));

            tokio::spawn({
                let address = peer.address.clone();
                let cancel = cancel.clone();
                let state = state.clone();

                async move {
                    run_peer_connection(address, peer_rx, state, cancel).await;
                }
            });

            peer_infos.push(PeerInfo {
                public_key: peer.public_key.clone(),
                tx: peer_tx,
            });
        }

        tokio::spawn({
            let cancel = cancel.clone();

            async move {
                loop {
                        let event = tokio::select! {
                            _ = cancel.cancelled() => break,
                            result = main_rx.recv() => {
                            match result {
                                Some(e) => e,
                                None => break,
                            }
                        }
                    };

                    let Some(active_peer) = active_state.get_active_peer() else {
                        continue;
                    };

                    for peer_info in &peer_infos {
                        if peer_info.public_key == active_peer {
                            if let Err(e) = peer_info.tx.try_send(event) {
                                warn!("scroll: Dropped scroll event: {}", e);
                            }
                        }
                    }
                }
            }
        });

        (main_tx, peer_states)
    }

    #[allow(dead_code)]
    pub fn atomic_to_peer_state(state: &AtomicU8) -> PeerState {
        match state.load(Ordering::Relaxed) {
            STATE_CONNECTED => PeerState::Connected,
            STATE_CONNECTING => PeerState::Connecting,
            _ => PeerState::Disconnected,
        }
    }
}

async fn run_peer_connection(
    address: String,
    mut rx: mpsc::Receiver<ScrollEvent>,
    state: Arc<AtomicU8>,
    cancel: CancellationToken,
) {
    loop {
        state.store(STATE_CONNECTING, Ordering::Relaxed);

        let mut client = tokio::select! {
            _ = cancel.cancelled() => return,
            result = connect(&address) => {
                match result {
                    Ok(client) => client,
                    Err(e) => {
                        error(&e);
                        state.store(STATE_DISCONNECTED, Ordering::Relaxed);
                        sleep(Duration::from_millis(RECONNECT_DELAY_MS)).await;
                        continue;
                    }
                }
            }
        };

        let (stream_tx, stream_rx) = mpsc::channel(CHANNEL_CAPACITY);
        let out_stream = ReceiverStream::new(stream_rx);

        let rpc_address = address.clone();
        let rpc_state = state.clone();
        let mut rpc_handle = tokio::spawn(async move {
            let result = client.scroll(out_stream).await;
            if let Err(e) = result {
                let e = Error::wrap(e, ErrorKind::Network)
                    .with_msg("transport: Scroll stream failed")
                    .with_ctx("address", &rpc_address);
                error(&e);
            }
            rpc_state.store(STATE_DISCONNECTED, Ordering::Relaxed);
        });

        state.store(STATE_CONNECTED, Ordering::Relaxed);

        info!("Scroll connection established to {}", address);

        loop {
            let event = tokio::select! {
                _ = cancel.cancelled() => {
                    rpc_handle.abort();
                    return;
                }
                _ = &mut rpc_handle => {
                    break;
                }
                result = rx.recv() => {
                    match result {
                        Some(event) => event,
                        None => {
                            rpc_handle.abort();
                            return;
                        }
                    }
                }
            };

            if stream_tx.send(event).await.is_err() {
                state.store(STATE_DISCONNECTED, Ordering::Relaxed);
                break;
            }
        }

        info!("Scroll connection lost to {}, reconnecting...", address);
        sleep(Duration::from_millis(RECONNECT_DELAY_MS)).await;
    }
}

async fn connect(address: &str) -> Result<SynqServiceClient<Channel>> {
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
