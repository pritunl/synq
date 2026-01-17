use std::sync::Arc;

use tokio::sync::mpsc;
use tonic::transport::Channel;

use crate::errors::{error, trace};
use crate::errors::{Result, Error, ErrorKind};
use crate::crypto;
use crate::crypto::KeyStore;
use crate::synq::{
    synq_service_client::SynqServiceClient,
    ClipboardEvent,
};

pub struct ClipboardSendEvent {
    pub peer_address: String,
    pub peer_public_key: String,
    pub text: String,
}

pub struct ClipboardTransport;

impl ClipboardTransport {
    pub fn start(
        key_store: Arc<KeyStore>,
        public_key: String,
    ) -> mpsc::Sender<ClipboardSendEvent> {
        let (tx, mut rx) = mpsc::channel::<ClipboardSendEvent>(16);

        tokio::spawn(async move {
            while let Some(event) = rx.recv().await {
                if let Err(e) = send_clipboard(
                    &key_store,
                    &public_key,
                    &event.peer_address,
                    &event.peer_public_key,
                    &event.text,
                ).await {
                    error(&e);
                }
            }
        });

        tx
    }
}

async fn send_clipboard(
    key_store: &KeyStore,
    our_public_key: &str,
    peer_address: &str,
    peer_public_key: &str,
    clipboard_text: &str,
) -> Result<()> {
    let encrypted = crypto::encrypt(
        key_store,
        peer_public_key,
        clipboard_text,
    )?;

    let event = ClipboardEvent {
        client: our_public_key.to_string(),
        data: encrypted.into_bytes(),
    };

    let mut client = connect(peer_address).await?;

    client.clipboard(event)
        .await
        .map_err(|e| Error::wrap(e, ErrorKind::Network)
            .with_msg("transport: Failed to send clipboard")
            .with_ctx("address", peer_address))?;

    trace!("Clipboard sent to {}", peer_address);

    Ok(())
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
