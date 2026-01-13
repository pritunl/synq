use std::sync::Arc;
use std::sync::atomic::{AtomicU8, Ordering};

use tokio::sync::mpsc;
use tokio::time::{sleep, Duration};
use tokio_stream::wrappers::ReceiverStream;
use tokio_util::sync::CancellationToken;
use tonic::transport::Channel;

use crate::errors::{error, info};
use crate::errors::{Result, Error, ErrorKind};
use crate::config::PeerConfig;
use crate::synq::{
    synq_service_client::SynqServiceClient,
    ScrollEvent,
};
use super::PeerState;

const CHANNEL_CAPACITY: usize = 32;
const RECONNECT_DELAY_MS: u64 = 1000;

const STATE_DISCONNECTED: u8 = 0;
const STATE_CONNECTING: u8 = 1;
const STATE_CONNECTED: u8 = 2;

pub struct ScrollTransport;

impl ScrollTransport {
    pub fn start(
        peers: &[PeerConfig],
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

        let mut peer_txs: Vec<mpsc::Sender<ScrollEvent>> = Vec::with_capacity(
            scroll_peers.len());
        let mut peer_states = Vec::with_capacity(scroll_peers.len());

        for peer in scroll_peers {
            let (peer_tx, peer_rx) = mpsc::channel(CHANNEL_CAPACITY);
            let state = Arc::new(AtomicU8::new(STATE_DISCONNECTED));

            peer_states.push((peer.address.clone(), state.clone()));

            let address = peer.address.clone();
            let peer_cancel = cancel.clone();
            let peer_state = state.clone();
            tokio::spawn(async move {
                run_peer_connection(address, peer_rx, peer_state, peer_cancel).await;
            });

            peer_txs.push(peer_tx);
        }

        let fanout_cancel = cancel.clone();
        tokio::spawn(async move {
            loop {
                let event = tokio::select! {
                    _ = fanout_cancel.cancelled() => break,
                    result = main_rx.recv() => {
                        match result {
                            Some(e) => e,
                            None => break,
                        }
                    }
                };

                for tx in &peer_txs {
                    let _ = tx.try_send(event.clone());
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

        let response = tokio::select! {
            _ = cancel.cancelled() => return,
            result = client.scroll(out_stream) => {
                match result {
                    Ok(response) => response,
                    Err(e) => {
                        let e = Error::wrap(e, ErrorKind::Network)
                            .with_msg("transport: Failed to start scroll stream")
                            .with_ctx("address", &address);
                        error(&e);
                        state.store(STATE_DISCONNECTED, Ordering::Relaxed);
                        sleep(Duration::from_millis(RECONNECT_DELAY_MS)).await;
                        continue;
                    }
                }
            }
        };

        state.store(STATE_CONNECTED, Ordering::Relaxed);

        info!("Scroll connection established to {}", address);

        drop(response);

        loop {
            let event = tokio::select! {
                _ = cancel.cancelled() => return,
                result = rx.recv() => {
                    match result {
                        Some(event) => event,
                        None => return,
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
