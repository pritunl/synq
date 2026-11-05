use crate::errors::{error, info, warn, trace};
use crate::errors::{Error, ErrorKind};
use crate::config::Config;
use crate::clipboard;
use crate::transport::{Transport};

pub(crate) async fn run_clipboard_source(
    config: Config,
    transport: Transport,
) {
    info!("Starting clipboard source");

    let cancel = transport.cancel_token();
    let mut clipboard_rx = match clipboard::watch_clipboard().await {
        Ok(rx) => rx,
        Err(e) => {
            let e = Error::wrap(e, ErrorKind::Exec)
                .with_msg("daemon: Failed to start clipboard watcher");
            error(&e);
            std::process::exit(1);
        }
    };

    loop {
        tokio::select! {
            _ = cancel.cancelled() => break,
            change = clipboard_rx.recv() => {
                if change.is_none() {
                    if cancel.is_cancelled() {
                        break;
                    }

                    // Exit so the service manager restarts instead of running without sharing
                    let e = Error::new(ErrorKind::Exec)
                        .with_msg("daemon: Clipboard watcher stopped unexpectedly");
                    error(&e);
                    std::process::exit(1);
                }
            }
        };

        let clipboard_text = match clipboard::get_clipboard().await {
            Ok(text) => text,
            Err(e) => {
                let e = Error::wrap(e, ErrorKind::Read)
                    .with_msg("daemon: Failed to get clipboard");
                warn!(?e);
                continue;
            }
        };

        if transport.is_recent_peer_clipboard(&clipboard_text) {
            trace!("Ignoring clipboard text recently received from a peer");
            continue;
        }

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
