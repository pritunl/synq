use std::sync::Mutex;

use crate::errors::{trace, info, warn, error};
use tokio::sync::mpsc;
use x11rb::{
    connection::Connection,
    protocol::{
        xfixes::{self, ConnectionExt as XFixesConnectionExt},
        xproto::{
            Atom, ConnectionExt, CreateWindowAux, WindowClass,
        },
        Event,
    },
    rust_connection::RustConnection,
    COPY_DEPTH_FROM_PARENT,
};

use crate::errors::{Result, Error, ErrorKind};

#[derive(Clone, Debug)]
pub struct ClipboardChange {
}

struct XlibState {
    conn: RustConnection,
    clip_atom: Atom,
    last_timestamp: Mutex<u32>,
}

impl XlibState {
    fn new() -> Result<Self> {
        let (conn, screen_num) = RustConnection::connect(None)
            .map_err(|e| Error::wrap(e, ErrorKind::Network)
                .with_msg("clipboard: Failed to connect to xlib display"))?;

        let screen = &conn.setup().roots[screen_num];
        let window = conn.generate_id()
            .map_err(|e| Error::wrap(e, ErrorKind::Network)
                .with_msg("clipboard: Failed to generate window ID"))?;

        conn.create_window(
            COPY_DEPTH_FROM_PARENT,
            window,
            screen.root,
            0,
            0,
            1,
            1,
            0,
            WindowClass::INPUT_OUTPUT,
            screen.root_visual,
            &CreateWindowAux::new(),
        )
        .map_err(|e| Error::wrap(e, ErrorKind::Network)
            .with_msg("clipboard: Failed to create window"))?;

        conn.flush()
            .map_err(|e| Error::wrap(e, ErrorKind::Network)
                .with_msg("clipboard: Failed to flush connection"))?;

        let clip_atom = conn.intern_atom(false, b"CLIPBOARD")
            .map_err(|e| Error::wrap(e, ErrorKind::Network)
                .with_msg("clipboard: Failed to intern atom"))?
            .reply()
            .map_err(|e| Error::wrap(e, ErrorKind::Network)
                .with_msg("clipboard: Failed to get atom reply"))?
            .atom;

        let xfixes_version = conn.xfixes_query_version(5, 0)
            .map_err(|e| Error::wrap(e, ErrorKind::Network)
                .with_msg("clipboard: Failed to query XFixes extension"))?
            .reply()
            .map_err(|e| Error::wrap(e, ErrorKind::Network)
                .with_msg("clipboard: XFixes extension not available"))?;

        trace!(
            "XFixes version: {}.{}",
            xfixes_version.major_version,
            xfixes_version.minor_version,
        );

        // Checking the request reports a failed subscription before the watch loop starts
        conn.xfixes_select_selection_input(
            window,
            clip_atom,
            xfixes::SelectionEventMask::SET_SELECTION_OWNER,
        )
        .map_err(|e| Error::wrap(e, ErrorKind::Network)
            .with_msg("clipboard: Failed to select clipboard input"))?
        .check()
        .map_err(|e| Error::wrap(e, ErrorKind::Network)
            .with_msg("clipboard: XFixes clipboard subscription failed"))?;

        Ok(Self {
            conn,
            clip_atom,
            last_timestamp: Mutex::new(0),
        })
    }

    fn watch_clipboard(&self, tx: mpsc::Sender<ClipboardChange>) -> Result<()> {
        loop {
            let event = self.conn.wait_for_event()
                .map_err(|e| Error::wrap(e, ErrorKind::Network)
                    .with_msg("clipboard: Failed to wait for event"))?;

            match event {
                Event::XfixesSelectionNotify(notify) => {
                    if notify.selection == self.clip_atom {
                        let mut last_ts = self.last_timestamp.lock().unwrap();
                        if notify.timestamp == *last_ts {
                            trace!(
                                "Ignoring duplicate clipboard event at timestamp {}",
                                notify.timestamp,
                            );
                            continue;
                        }
                        *last_ts = notify.timestamp;
                        drop(last_ts);

                        trace!("Clipboard changed at timestamp {}", notify.timestamp);

                        let change = ClipboardChange {};

                        tx.blocking_send(change)
                            .map_err(|e| Error::wrap(e, ErrorKind::Network)
                                .with_msg("clipboard: Failed to send change event"))?;
                    }
                }
                _ => {
                    trace!("Received non-clipboard event");
                }
            }
        }
    }
}

pub async fn watch_clipboard() -> Result<mpsc::Receiver<ClipboardChange>> {
    if std::env::var_os("WAYLAND_DISPLAY").is_some_and(|value| !value.is_empty()) {
        if let Some(rx) = super::wayland::try_watch_clipboard().await? {
            return Ok(rx);
        }

        // GNOME offers no data control protocol but bridges the Xwayland clipboard
        // without the focus limits of wlroots, other Wayland failures never fall back
        warn!("Compositor does not support data control, using Xwayland clipboard");

        return watch_x11_clipboard().await
            .map_err(|e| Error::wrap(e, ErrorKind::Connection)
                .with_msg("clipboard: No data control and Xwayland clipboard unavailable"));
    }

    watch_x11_clipboard().await
}

async fn watch_x11_clipboard() -> Result<mpsc::Receiver<ClipboardChange>> {
    let (tx, rx) = mpsc::channel(32);

    let state = tokio::task::spawn_blocking(XlibState::new)
        .await
        .map_err(|e| Error::wrap(e, ErrorKind::Exec)
            .with_msg("clipboard: Task join failed"))??;

    info!("Started X11 clipboard watcher");

    tokio::task::spawn_blocking(move || {
        if let Err(e) = state.watch_clipboard(tx) {
            error(&e);
        }
    });

    Ok(rx)
}
