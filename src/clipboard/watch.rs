use std::sync::Mutex;

use crate::errors::{trace, error};
use tokio::sync::mpsc;
use x11rb::{
    connection::Connection,
    protocol::{
        xfixes::{self, ConnectionExt as XFixesConnectionExt},
        xproto::{
            Atom, ConnectionExt, CreateWindowAux, Window, WindowClass,
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

struct X11State {
    conn: RustConnection,
    window: Window,
    clip_atom: Atom,
    last_timestamp: Mutex<u32>,
}

impl X11State {
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

        Ok(Self {
            conn,
            window,
            clip_atom,
            last_timestamp: Mutex::new(0),
        })
    }

    fn watch_clipboard(&self, tx: mpsc::Sender<ClipboardChange>) -> Result<()> {
        let xfixes_version = self.conn.xfixes_query_version(5, 0)
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

        self.conn.xfixes_select_selection_input(
            self.window,
            self.clip_atom,
            xfixes::SelectionEventMask::SET_SELECTION_OWNER,
        )
        .map_err(|e| Error::wrap(e, ErrorKind::Network)
            .with_msg("clipboard: Failed to select clipboard input"))?;

        self.conn.flush()
            .map_err(|e| Error::wrap(e, ErrorKind::Network)
                .with_msg("clipboard: Failed to flush after selecting input"))?;

        trace!("Started watching clipboard");

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
    let (tx, rx) = mpsc::channel(32);
    let (init_tx, mut init_rx) = mpsc::channel::<Result<()>>(1);

    tokio::task::spawn_blocking(move || {
        trace!("Initializing X11 connection");

        let state = match X11State::new() {
            Ok(state) => {
                if let Err(e) = init_tx.blocking_send(Ok(())) {
                    let e = Error::wrap(e, ErrorKind::Network)
                        .with_msg("clipboard: Failed to send initialization success");
                    error(&e);
                }
                state
            }
            Err(e) => {
                if let Err(send_err) = init_tx.blocking_send(Err(e)) {
                    let e = Error::wrap(send_err, ErrorKind::Network)
                        .with_msg("clipboard: Failed to send initialization error");
                    error(&e);
                }
                return;
            }
        };

        trace!("Starting xlib clipboard watch loop");

        if let Err(e) = state.watch_clipboard(tx) {
            error(&e);
        }
    });

    init_rx.recv().await
        .ok_or_else(|| Error::new(ErrorKind::Network)
            .with_msg("clipboard: Initialization channel closed unexpectedly"))??;

    Ok(rx)
}
