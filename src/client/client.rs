use tracing::{error, info};
use tokio::sync::{mpsc, broadcast};
use tokio_stream::wrappers::ReceiverStream;
use tokio_stream::StreamExt;
use tonic::transport::Channel;

use crate::errors::{Result, Error, ErrorKind};
use crate::synq::{
    synq_service_client::SynqServiceClient,
    ScrollEvent, ClipboardEvent,
};

#[derive(Debug, Clone)]
pub struct Client {
    peers: Vec<String>,
    public_key: String,
}

impl Client {
    async fn connect_to_peer(&self, peer: &str) -> Result<SynqServiceClient<Channel>> {
        let url = if peer.starts_with("http://") || peer.starts_with("https://") {
            peer.to_string()
        } else {
            format!("http://{}", peer)
        };

        let channel = Channel::from_shared(url)
            .map_err(|e| Error::wrap(e, ErrorKind::Network)
                .with_msg("client: Invalid peer address")
                .with_ctx("address", peer))?
            .connect()
            .await
            .map_err(|e| Error::wrap(e, ErrorKind::Network)
                .with_msg("client: Failed to connect")
                .with_ctx("address", peer))?;

        Ok(SynqServiceClient::new(channel))
    }

    pub async fn send_scroll(&self, delta_x: i32, delta_y: i32) -> Result<()> {
        let event = ScrollEvent { delta_x, delta_y };

        for peer in &self.peers {
            let mut client = self.connect_to_peer(peer).await?;

            tokio::spawn({
                let event = event.clone();

                async move {
                    let (tx, rx) = mpsc::channel(1);

                    if let Err(e) = tx.send(event).await {
                        let e = Error::wrap(e, ErrorKind::Network)
                            .with_msg("client: Failed to queue scroll event");
                        error!(?e);
                        return;
                    }
                    drop(tx);

                    let out_stream = ReceiverStream::new(rx);

                    match client.scroll(out_stream).await {
                        Ok(response) => {
                            let mut in_stream = response.into_inner();
                            while let Some(result) = in_stream.next().await {
                                if let Err(e) = result {
                                    let e = Error::wrap(e, ErrorKind::Network)
                                        .with_msg("client: Scroll stream error");
                                    error!(?e);
                                    break;
                                }
                            }
                        }
                        Err(e) => {
                            let e = Error::wrap(e, ErrorKind::Network)
                                .with_msg("client: Failed to send scroll");
                            error!(?e);
                        }
                    }
                }
            });
        }

        Ok(())
    }

    pub async fn send_clipboard(&self, data: Vec<u8>) -> Result<()> {
        let event = ClipboardEvent {
            client: self.public_key.clone(),
            data,
        };

        for peer in &self.peers {
            let mut client = self.connect_to_peer(peer).await?;

            tokio::spawn({
                let event = event.clone();

                async move {
                    let (tx, rx) = mpsc::channel(1);

                    if let Err(e) = tx.send(event).await {
                        let e = Error::wrap(e, ErrorKind::Network)
                            .with_msg("client: Failed to queue clipboard event");
                        error!(?e);
                        return;
                    }
                    drop(tx);

                    let out_stream = ReceiverStream::new(rx);

                    match client.clipboard(out_stream).await {
                        Ok(response) => {
                            let mut in_stream = response.into_inner();
                            while let Some(result) = in_stream.next().await {
                                if let Err(e) = result {
                                    let e = Error::wrap(e, ErrorKind::Network)
                                        .with_msg("client: Clipboard stream error");
                                    error!(?e);
                                    break;
                                }
                            }
                        }
                        Err(e) => {
                            let e = Error::wrap(e, ErrorKind::Network)
                                .with_msg("client: Failed to send clipboard");
                            error!(?e);
                        }
                    }
                }
            });
        }

        Ok(())
    }

    pub async fn start_scroll_stream(
        &self, mut rx: mpsc::Receiver<ScrollEvent>,
    ) -> Result<()> {
        let (broadcast_tx, _) = broadcast::channel(128);

        tokio::spawn({
            let broadcast_tx = broadcast_tx.clone();

            async move {
                while let Some(event) = rx.recv().await {
                    let _ = broadcast_tx.send(event);
                }
            }
        });

        for peer in &self.peers {
            let mut client = self.connect_to_peer(peer).await?;
            let (tx, stream_rx) = mpsc::channel(128);

            tokio::spawn({
                let peer = peer.clone();

                async move {
                    let out_stream = ReceiverStream::new(stream_rx);

                    match client.scroll(out_stream).await {
                        Ok(response) => {
                            info!("Scroll stream connected to {}", peer);
                            let mut in_stream = response.into_inner();
                            while let Some(result) = in_stream.next().await {
                                if let Err(e) = result {
                                    let e = Error::wrap(e, ErrorKind::Network)
                                        .with_msg("client: Scroll stream error")
                                        .with_ctx("peer", &peer);
                                    error!(?e);
                                    break;
                                }
                            }
                            info!("Scroll stream disconnected from {}", peer);
                        }
                        Err(e) => {
                            let e = Error::wrap(e, ErrorKind::Network)
                                .with_msg("client: Failed to establish scroll stream")
                                .with_ctx("peer", &peer);
                            error!(?e);
                        }
                    }
                }
            });

            tokio::spawn({
                let broadcast_rx = broadcast_tx.subscribe();

                async move {
                    let mut broadcast_rx = broadcast_rx;
                    while let Ok(event) = broadcast_rx.recv().await {
                        if let Err(e) = tx.send(event).await {
                            let e = Error::wrap(e, ErrorKind::Network)
                                .with_msg("client: Failed to forward scroll event");
                            error!(?e);
                            break;
                        }
                    }
                }
            });
        }

        Ok(())
    }

    pub async fn start_clipboard_stream(
        &self, mut rx: mpsc::Receiver<ClipboardEvent>
    ) -> Result<()> {
        let (broadcast_tx, _) = broadcast::channel(128);

        tokio::spawn({
            let broadcast_tx = broadcast_tx.clone();

            async move {
                while let Some(event) = rx.recv().await {
                    let _ = broadcast_tx.send(event);
                }
            }
        });

        for peer in &self.peers {
            let mut client = self.connect_to_peer(peer).await?;
            let (tx, stream_rx) = mpsc::channel(128);

            tokio::spawn({
                let peer = peer.clone();

                async move {
                    let out_stream = ReceiverStream::new(stream_rx);

                    match client.clipboard(out_stream).await {
                        Ok(response) => {
                            info!("Clipboard stream connected to {}", peer);
                            let mut in_stream = response.into_inner();
                            while let Some(result) = in_stream.next().await {
                                if let Err(e) = result {
                                    let e = Error::wrap(e, ErrorKind::Network)
                                        .with_msg("client: Clipboard stream error")
                                        .with_ctx("peer", &peer);
                                    error!(?e);
                                    break;
                                }
                            }
                            info!("Clipboard stream disconnected from {}", peer);
                        }
                        Err(e) => {
                            let e = Error::wrap(e, ErrorKind::Network)
                                .with_msg("client: Failed to establish clipboard stream")
                                .with_ctx("peer", &peer);
                            error!(?e);
                        }
                    }
                }
            });

            tokio::spawn({
                let broadcast_rx = broadcast_tx.subscribe();

                async move {
                    let mut broadcast_rx = broadcast_rx;
                    while let Ok(event) = broadcast_rx.recv().await {
                        if let Err(e) = tx.send(event).await {
                            let e = Error::wrap(e, ErrorKind::Network)
                                .with_msg("client: Failed to forward clipboard event");
                            error!(?e);
                            break;
                        }
                    }
                }
            });
        }

        Ok(())
    }

    pub async fn run(public_key: String, peers: Vec<String>) -> Result<()> {
        let client = Client{
            public_key,
            peers,
        };

        info!("Synq client started with {} peers", client.peers.len());

        let (_scroll_tx, scroll_rx) = mpsc::channel(128);
        let (_clipboard_tx, clipboard_rx) = mpsc::channel(128);

        client.start_scroll_stream(scroll_rx).await?;
        client.start_clipboard_stream(clipboard_rx).await?;

        tokio::spawn({
            let client = client.clone();

            async move {
                let mut interval = tokio::time::interval(tokio::time::Duration::from_secs(10));
                interval.tick().await;

                loop {
                    interval.tick().await;
                    let test_data = b"Test clipboard event".to_vec();
                    info!("Sending test clipboard event");
                    if let Err(e) = client.send_clipboard(test_data).await {
                        error!(?e);
                    }
                }
            }
        });

        tokio::signal::ctrl_c().await
            .map_err(|e| Error::wrap(e, ErrorKind::Read)
                .with_msg("client: Failed to listen for shutdown signal"))?;

        info!("Shutting down client");
        Ok(())
    }
}
