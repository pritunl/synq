use std::sync::atomic::Ordering;

use tokio_util::sync::CancellationToken;

use crate::errors::{error, info, warn, trace};
use crate::errors::{Error, ErrorKind};
use crate::config::Config;
use crate::clipboard;
use crate::transport::{Transport};
use crate::utils;

use super::constants::CLIPBOARD_TTL;

pub(crate) async fn run_clipboard_source(
    config: Config,
    transport: Transport,
    cancel: CancellationToken,
) {
    info!("Starting clipboard source");

    let mut clipboard_rx = match clipboard::watch_clipboard().await {
        Ok(rx) => rx,
        Err(e) => {
            let e = Error::wrap(e, ErrorKind::Exec)
                .with_msg("daemon: Failed to start clipboard watcher");
            error(&e);
            return;
        }
    };

    loop {
        tokio::select! {
            _ = cancel.cancelled() => break,
            change = clipboard_rx.recv() => {
                if change.is_none() {
                    break;
                }
            }
        };

        let last_set = transport.last_set_clipboard().load(Ordering::SeqCst);
        if utils::mono_time_ms().saturating_sub(last_set) < CLIPBOARD_TTL {
            trace!("Ignoring clipboard change within debounce window");
            continue;
        }

        let clipboard_text = match clipboard::get_clipboard().await {
            Ok(text) => text,
            Err(e) => {
                let e = Error::wrap(e, ErrorKind::Read)
                    .with_msg("daemon: Failed to get clipboard");
                warn!(?e);
                continue;
            }
        };

        trace!("Clipboard changed, sending to peers");

        for peer in &config.peers {
            if peer.clipboard_destination {
                if !transport.send_clipboard(
                    peer.address.clone(),
                    peer.public_key.clone(),
                    clipboard_text.clone(),
                ) {
                    warn!("Clipboard send dropped for {}", peer.address);
                }
            }
        }
    }
}
