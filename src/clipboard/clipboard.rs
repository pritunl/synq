use std::io::Read;
use std::os::unix::fs::MetadataExt;
use std::path::PathBuf;
use std::sync::Mutex;

use arboard::{Clipboard, SetExtLinux};
use wl_clipboard_rs::{copy, paste, utils};

use crate::errors::{Result, Error, ErrorKind, error, warn};

#[derive(Clone, Copy)]
enum Backend {
    Wayland,
    X11,
}

// A replacement compositor can expose different protocols at the same socket path.
// Include the change time because an unlinked inode number can be reused.
type SocketId = (u64, u64, i64, i64);
static BACKEND: Mutex<Option<(SocketId, Backend)>> = Mutex::new(None);

fn backend() -> Result<Backend> {
    let Some(display) = std::env::var_os("WAYLAND_DISPLAY").filter(|v| !v.is_empty()) else {
        return Ok(Backend::X11);
    };

    let path = PathBuf::from(display);
    let path = if path.is_absolute() {
        Some(path)
    } else {
        std::env::var_os("XDG_RUNTIME_DIR").map(|dir| PathBuf::from(dir).join(path))
    };
    let socket = path.and_then(|path| path.metadata().ok())
        .map(|m| (m.dev(), m.ino(), m.ctime(), m.ctime_nsec()));

    let mut cached = BACKEND.lock().unwrap_or_else(|e| e.into_inner());
    if let (Some(socket), Some((previous, backend))) = (socket, *cached)
        && socket == previous
    {
        return Ok(backend);
    }

    // Both Ok values mean data control is present. The boolean describes primary selection.
    let backend = match utils::is_primary_selection_supported() {
        Ok(_) => Backend::Wayland,
        Err(utils::PrimarySelectionCheckError::MissingProtocol) => Backend::X11,
        Err(e) if std::env::var_os("DISPLAY").is_some_and(|v| !v.is_empty()) => {
            // A receive-only daemon can still use X11 while Wayland is unavailable.
            // Retry the Wayland probe on the next request.
            *cached = None;
            warn!("Wayland clipboard probe failed, trying X11 for this request: {e}");
            return Ok(Backend::X11);
        }
        Err(e) => return Err(Error::wrap(e, ErrorKind::Connection)
            .with_msg("clipboard: Failed to check Wayland data control")),
    };
    *cached = socket.map(|id| (id, backend));
    Ok(backend)
}

pub async fn get_clipboard() -> Result<String> {
    tokio::task::spawn_blocking(|| match backend()? {
        Backend::Wayland => {
            let (mut contents, _) = paste::get_contents(
                paste::ClipboardType::Regular, paste::Seat::Unspecified, paste::MimeType::Text,
            ).map_err(|e| Error::wrap(e, ErrorKind::Read)
                .with_msg("clipboard: Failed to request Wayland clipboard text"))?;
            let mut text = String::new();
            contents.read_to_string(&mut text)
                .map_err(|e| Error::wrap(e, ErrorKind::Read)
                    .with_msg("clipboard: Failed to read Wayland clipboard text"))?;
            Ok(text)
        }
        Backend::X11 => Clipboard::new().and_then(|mut clipboard| clipboard.get_text())
            .map_err(|e| Error::wrap(e, ErrorKind::Read)
                .with_msg("clipboard: Failed to read X11 clipboard text")),
    })
    .await
    .map_err(|e| Error::wrap(e, ErrorKind::Read)
        .with_msg("clipboard: Task join failed"))?
}

fn write_text(text: String) -> Result<()> {
    match backend()? {
        Backend::Wayland => {
            let mut options = copy::Options::new();
            options.foreground(true);
            let source = copy::Source::Bytes(text.into_bytes().into_boxed_slice());
            options.copy(source, copy::MimeType::Text)
                .map_err(|e| Error::wrap(e, ErrorKind::Write)
                    .with_msg("clipboard: Failed to write Wayland clipboard text"))
        }
        Backend::X11 => Clipboard::new().and_then(|mut clipboard| clipboard.set().wait().text(text))
            .map_err(|e| Error::wrap(e, ErrorKind::Write)
                .with_msg("clipboard: Failed to write X11 clipboard text")),
    }
}

pub fn set_clipboard(text: String) {
    tokio::task::spawn_blocking(move || {
        if let Err(e) = write_text(text) {
            error(&e);
        }
    });
}
