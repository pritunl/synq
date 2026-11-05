use std::io;
use std::os::fd::AsFd;
use std::sync::atomic::{AtomicBool, Ordering};

use tokio::io::unix::AsyncFd;
use tokio::sync::mpsc;
use wayland_client::{
    Connection, Dispatch, EventQueue, Proxy, QueueHandle,
    backend::WaylandError,
    globals::{GlobalListContents, registry_queue_init},
    protocol::{wl_registry, wl_seat},
};
use wayland_protocols::ext::data_control::v1::client::{
    ext_data_control_device_v1::{self, ExtDataControlDeviceV1},
    ext_data_control_manager_v1::ExtDataControlManagerV1,
    ext_data_control_offer_v1::{self, ExtDataControlOfferV1},
};
use wayland_protocols_wlr::data_control::v1::client::{
    zwlr_data_control_device_v1::{self, ZwlrDataControlDeviceV1},
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

impl OfferData {
    fn offered(&self, mime_type: &str) {
        if is_text_mime(mime_type) {
            self.text.store(true, Ordering::SeqCst);
        }
    }
}

// The ext protocol is the standardized wlr successor, compositors may offer either
enum Manager {
    Ext(ExtDataControlManagerV1),
    Wlr(ZwlrDataControlManagerV1),
}

impl Manager {
    fn protocol(&self) -> &'static str {
        match self {
            Manager::Ext(_) => ExtDataControlManagerV1::interface().name,
            Manager::Wlr(_) => ZwlrDataControlManagerV1::interface().name,
        }
    }

    fn get_data_device(&self, seat: &wl_seat::WlSeat, qh: &QueueHandle<WaylandState>) {
        match self {
            Manager::Ext(manager) => {
                manager.get_data_device(seat, qh, ());
            }
            Manager::Wlr(manager) => {
                manager.get_data_device(seat, qh, ());
            }
        }
    }
}

enum Offer {
    Ext(ExtDataControlOfferV1),
    Wlr(ZwlrDataControlOfferV1),
}

impl Offer {
    fn is_text(&self) -> bool {
        let data = match self {
            Offer::Ext(offer) => offer.data::<OfferData>(),
            Offer::Wlr(offer) => offer.data::<OfferData>(),
        };
        data.is_some_and(|data| data.text.load(Ordering::SeqCst))
    }

    fn destroy(&self) {
        match self {
            Offer::Ext(offer) => offer.destroy(),
            Offer::Wlr(offer) => offer.destroy(),
        }
    }
}

struct WaylandState {
    selection: Option<Offer>,
    tx: mpsc::Sender<ClipboardChange>,
    ready: bool,
    finished: bool,
}

struct WaylandWatcher {
    connection: Connection,
    queue: EventQueue<WaylandState>,
    state: WaylandState,
    protocol: &'static str,
}

fn is_text_mime(mime_type: &str) -> bool {
    mime_type.starts_with("text/")
        || matches!(mime_type, "UTF8_STRING" | "STRING" | "TEXT")
}

impl WaylandState {
    fn select(&mut self, offer: Option<Offer>) {
        let is_text = offer.as_ref().is_some_and(Offer::is_text);

        if let Some(previous) = self.selection.take() {
            previous.destroy();
        }
        self.selection = offer;

        // Receiver reads the selection on notify, a full queue has a pending read
        if self.ready && is_text {
            let _ = self.tx.try_send(ClipboardChange {});
        }
    }
}

impl Dispatch<wl_registry::WlRegistry, GlobalListContents> for WaylandState {
    fn event(
        _: &mut Self,
        _: &wl_registry::WlRegistry,
        _: wl_registry::Event,
        _: &GlobalListContents,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        // Globals are collected by registry_queue_init, later changes are not tracked
    }
}

wayland_client::delegate_noop!(WaylandState: ignore wl_seat::WlSeat);
wayland_client::delegate_noop!(WaylandState: ignore ExtDataControlManagerV1);
wayland_client::delegate_noop!(WaylandState: ignore ZwlrDataControlManagerV1);

impl Dispatch<ExtDataControlOfferV1, OfferData> for WaylandState {
    fn event(
        _: &mut Self,
        _: &ExtDataControlOfferV1,
        event: ext_data_control_offer_v1::Event,
        data: &OfferData,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let ext_data_control_offer_v1::Event::Offer { mime_type } = event {
            data.offered(&mime_type);
        }
    }
}

impl Dispatch<ZwlrDataControlOfferV1, OfferData> for WaylandState {
    fn event(
        _: &mut Self,
        _: &ZwlrDataControlOfferV1,
        event: zwlr_data_control_offer_v1::Event,
        data: &OfferData,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let zwlr_data_control_offer_v1::Event::Offer { mime_type } = event {
            data.offered(&mime_type);
        }
    }
}

impl Dispatch<ExtDataControlDeviceV1, ()> for WaylandState {
    fn event(
        state: &mut Self,
        _: &ExtDataControlDeviceV1,
        event: ext_data_control_device_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            ext_data_control_device_v1::Event::Selection { id } => {
                state.select(id.map(Offer::Ext));
            }
            ext_data_control_device_v1::Event::PrimarySelection { id: Some(offer) } => {
                // Primary selection is not synced, release the offer right away
                offer.destroy();
            }
            ext_data_control_device_v1::Event::Finished => {
                state.finished = true;
            }
            _ => {}
        }
    }

    wayland_client::event_created_child!(WaylandState, ExtDataControlDeviceV1, [
        ext_data_control_device_v1::EVT_DATA_OFFER_OPCODE
            => (ExtDataControlOfferV1, OfferData::default()),
    ]);
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
                state.select(id.map(Offer::Wlr));
            }
            zwlr_data_control_device_v1::Event::PrimarySelection { id: Some(offer) } => {
                // Primary selection is not synced, release the offer right away
                offer.destroy();
            }
            zwlr_data_control_device_v1::Event::Finished => {
                state.finished = true;
            }
            _ => {}
        }
    }

    wayland_client::event_created_child!(WaylandState, ZwlrDataControlDeviceV1, [
        zwlr_data_control_device_v1::EVT_DATA_OFFER_OPCODE
            => (ZwlrDataControlOfferV1, OfferData::default()),
    ]);
}

impl WaylandWatcher {
    fn new(tx: mpsc::Sender<ClipboardChange>) -> Result<Option<Self>> {
        let connection = Connection::connect_to_env()
            .map_err(|e| Error::wrap(e, ErrorKind::Connection)
                .with_msg("clipboard: Failed to connect to Wayland display"))?;

        let (globals, mut queue) = registry_queue_init::<WaylandState>(&connection)
            .map_err(|e| Error::wrap(e, ErrorKind::Connection)
                .with_msg("clipboard: Failed to get Wayland globals"))?;
        let qh = queue.handle();

        // Prefer the standard protocol, version 1 of either covers the clipboard selection
        let manager = globals.bind(&qh, 1..=1, ()).map(Manager::Ext)
            .or_else(|_| globals.bind(&qh, 1..=1, ()).map(Manager::Wlr));

        // GNOME advertises neither protocol, the caller falls back to Xwayland
        let Ok(manager) = manager else {
            return Ok(None);
        };

        // First seat, same as arboard
        let seat: wl_seat::WlSeat = globals.bind(&qh, 1..=2, ())
            .map_err(|e| Error::wrap(e, ErrorKind::NotFound)
                .with_msg("clipboard: No Wayland seat available"))?;

        let mut state = WaylandState {
            selection: None,
            tx,
            ready: false,
            finished: false,
        };

        manager.get_data_device(&seat, &qh);
        queue.roundtrip(&mut state)
            .map_err(|e| Error::wrap(e, ErrorKind::Connection)
                .with_msg("clipboard: Failed to get Wayland data device"))?;

        if state.finished {
            return Err(Error::new(ErrorKind::Connection)
                .with_msg("clipboard: Wayland data device finished"));
        }

        // The initial selection from the roundtrip is a snapshot not a change
        state.ready = true;

        Ok(Some(Self {
            connection,
            queue,
            state,
            protocol: manager.protocol(),
        }))
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

// None means the compositor has no data control protocol, other failures are errors
pub(super) async fn try_watch_clipboard() -> Result<Option<mpsc::Receiver<ClipboardChange>>> {
    let (tx, rx) = mpsc::channel(32);

    let watcher = tokio::task::spawn_blocking(move || WaylandWatcher::new(tx))
        .await
        .map_err(|e| Error::wrap(e, ErrorKind::Exec)
            .with_msg("clipboard: Task join failed"))??;
    let Some(watcher) = watcher else {
        return Ok(None);
    };

    info!("Started Wayland clipboard watcher with {}", watcher.protocol);

    tokio::spawn(async move {
        if let Err(e) = watcher.run().await {
            error(&e);
        }
    });

    Ok(Some(rx))
}
