#[allow(clippy::module_inception)]
mod transport;
mod server;
mod scroll;
mod clipboard;
mod active;
mod constants;

pub use transport::{Transport, PeerState, ScrollInjectRx};
pub use active::{ActiveState, send_active_state};
