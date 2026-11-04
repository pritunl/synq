use std::io;
use std::os::fd::AsFd;
use std::sync::atomic::{AtomicBool, Ordering};

use tokio::io::unix::AsyncFd;
use tokio::sync::mpsc;
use wayland_client::{
    Connection, Dispatch, EventQueue, Proxy, QueueHandle,
    backend::WaylandError,
    protocol::{wl_registry, wl_seat},
};
use wayland_protocols_wlr::data_control::v1::client::{
    zwlr_data_control_device_v1::{self, ZwlrDataControlDeviceV1, EVT_DATA_OFFER_OPCODE},
    zwlr_data_control_manager_v1::ZwlrDataControlManagerV1,
    zwlr_data_control_offer_v1::{self, ZwlrDataControlOfferV1},
};

use crate::errors::{error, info};
use crate::errors::{Result, Error, ErrorKind};

use super::ClipboardChange;

#[derive(Default)]
struct OfferData {
    text: AtomicBool,
}

struct WaylandState {
    manager: Option<ZwlrDataControlManagerV1>,
    seat: Option<wl_seat::WlSeat>,
    selection: Option<ZwlrDataControlOfferV1>,
    tx: mpsc::Sender<ClipboardChange>,
    ready: bool,
    finished: bool,
}

struct WaylandWatcher {
    connection: Connection,
    queue: EventQueue<WaylandState>,
    state: WaylandState,
}

fn is_text_mime(mime_type: &str) -> bool {
    mime_type.starts_with("text/")
        || matches!(mime_type, "UTF8_STRING" | "STRING" | "TEXT")
}

impl Dispatch<wl_registry::WlRegistry, ()> for WaylandState {
    fn event(
        state: &mut Self,
        registry: &wl_registry::WlRegistry,
        event: wl_registry::Event,
        _: &(),
        _: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        let wl_registry::Event::Global { name, interface, version } = event else {
            return;
        };

        if interface == ZwlrDataControlManagerV1::interface().name
            && state.manager.is_none()
        {
            // Version 1 covers the clipboard without primary selection events
            state.manager = Some(registry.bind(name, version.min(1), qh, ()));
        } else if interface == wl_seat::WlSeat::interface().name
            && state.seat.is_none()
        {
            // First seat, same as arboard
            state.seat = Some(registry.bind(name, version.min(2), qh, ()));
        }
    }
}

wayland_client::delegate_noop!(WaylandState: ignore wl_seat::WlSeat);
wayland_client::delegate_noop!(WaylandState: ignore ZwlrDataControlManagerV1);

impl Dispatch<ZwlrDataControlOfferV1, OfferData> for WaylandState {
    fn event(
        _: &mut Self,
        _: &ZwlrDataControlOfferV1,
        event: zwlr_data_control_offer_v1::Event,
        data: &OfferData,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            zwlr_data_control_offer_v1::Event::Offer { mime_type }
                if is_text_mime(&mime_type) =>
            {
                data.text.store(true, Ordering::SeqCst);
            }
            _ => {}
        }
    }
}

impl Dispatch<ZwlrDataControlDeviceV1, ()> for WaylandState {
    fn event(
        state: &mut Self,
        _: &ZwlrDataControlDeviceV1,
        event: zwlr_data_control_device_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            zwlr_data_control_device_v1::Event::Selection { id } => {
                let is_text = id.as_ref()
                    .and_then(|offer| offer.data::<OfferData>())
                    .is_some_and(|data| data.text.load(Ordering::SeqCst));

                if let Some(previous) = state.selection.take() {
                    previous.destroy();
                }
                state.selection = id;

                // Receiver reads the selection on notify, a full queue has a pending read
                if state.ready && is_text {
                    let _ = state.tx.try_send(ClipboardChange {});
                }
            }
            zwlr_data_control_device_v1::Event::Finished => {
                state.finished = true;
            }
            _ => {}
        }
    }

    wayland_client::event_created_child!(WaylandState, ZwlrDataControlDeviceV1, [
        EVT_DATA_OFFER_OPCODE => (ZwlrDataControlOfferV1, OfferData::default()),
    ]);
}

impl WaylandWatcher {
    fn new(tx: mpsc::Sender<ClipboardChange>) -> Result<Self> {
        let connection = Connection::connect_to_env()
            .map_err(|e| Error::wrap(e, ErrorKind::Connection)
                .with_msg("clipboard: Failed to connect to Wayland display"))?;

        let mut queue = connection.new_event_queue();
        let qh = queue.handle();
        let mut state = WaylandState {
            manager: None,
            seat: None,
            selection: None,
            tx,
            ready: false,
            finished: false,
        };

        connection.display().get_registry(&qh, ());
        queue.roundtrip(&mut state)
            .map_err(|e| Error::wrap(e, ErrorKind::Connection)
                .with_msg("clipboard: Failed to get Wayland globals"))?;

        let manager = state.manager.as_ref()
            .ok_or_else(|| Error::new(ErrorKind::NotFound)
                .with_msg("clipboard: Compositor does not support wlr data control"))?;
        let seat = state.seat.as_ref()
            .ok_or_else(|| Error::new(ErrorKind::NotFound)
                .with_msg("clipboard: No Wayland seat available"))?;

        manager.get_data_device(seat, &qh, ());
        queue.roundtrip(&mut state)
            .map_err(|e| Error::wrap(e, ErrorKind::Connection)
                .with_msg("clipboard: Failed to get Wayland data device"))?;

        if state.finished {
            return Err(Error::new(ErrorKind::Connection)
                .with_msg("clipboard: Wayland data device finished"));
        }

        // The initial selection from the roundtrip is a snapshot not a change
        state.ready = true;

        Ok(Self {
            connection,
            queue,
            state,
        })
    }

    async fn run(mut self) -> Result<()> {
        let fd = self.connection.as_fd().try_clone_to_owned()
            .map_err(|e| Error::wrap(e, ErrorKind::Connection)
                .with_msg("clipboard: Failed to clone Wayland socket"))?;
        let socket = AsyncFd::new(fd)
            .map_err(|e| Error::wrap(e, ErrorKind::Connection)
                .with_msg("clipboard: Failed to register Wayland socket"))?;

        loop {
            self.queue.dispatch_pending(&mut self.state)
                .map_err(|e| Error::wrap(e, ErrorKind::Connection)
                    .with_msg("clipboard: Failed to dispatch Wayland events"))?;

            if self.state.finished {
                return Err(Error::new(ErrorKind::Connection)
                    .with_msg("clipboard: Wayland data device finished"));
            }

            loop {
                match self.queue.flush() {
                    Ok(()) => break,
                    Err(WaylandError::Io(e)) if e.kind() == io::ErrorKind::WouldBlock => {
                        tokio::select! {
                            _ = self.state.tx.closed() => return Ok(()),
                            result = socket.writable() => {
                                let mut guard = result
                                    .map_err(|e| Error::wrap(e, ErrorKind::Connection)
                                        .with_msg("clipboard: Failed to wait for Wayland socket"))?;
                                guard.clear_ready();
                            }
                        }
                    }
                    Err(e) => {
                        return Err(Error::wrap(e, ErrorKind::Connection)
                            .with_msg("clipboard: Failed to flush Wayland requests"));
                    }
                }
            }

            let Some(read) = self.queue.prepare_read() else {
                continue;
            };

            tokio::select! {
                _ = self.state.tx.closed() => return Ok(()),
                result = socket.readable() => {
                    let mut guard = result
                        .map_err(|e| Error::wrap(e, ErrorKind::Connection)
                            .with_msg("clipboard: Failed to wait for Wayland socket"))?;

                    match read.read() {
                        Ok(_) => {}
                        Err(WaylandError::Io(e)) if e.kind() == io::ErrorKind::WouldBlock => {
                            guard.clear_ready();
                        }
                        Err(e) => {
                            return Err(Error::wrap(e, ErrorKind::Connection)
                                .with_msg("clipboard: Failed to read Wayland events"));
                        }
                    }
                }
            }
        }
    }
}

pub(super) async fn watch_wayland_clipboard() -> Result<mpsc::Receiver<ClipboardChange>> {
    let (tx, rx) = mpsc::channel(32);

    let watcher = tokio::task::spawn_blocking(move || WaylandWatcher::new(tx))
        .await
        .map_err(|e| Error::wrap(e, ErrorKind::Exec)
            .with_msg("clipboard: Task join failed"))??;

    info!("Started Wayland clipboard watcher");

    tokio::spawn(async move {
        if let Err(e) = watcher.run().await {
            error(&e);
        }
    });

    Ok(rx)
}
