use arboard::{Clipboard, SetExtLinux, LinuxClipboardKind};

use crate::errors::{Result, Error, ErrorKind, error};

pub async fn get_clipboard() -> Result<String> {
    tokio::task::spawn_blocking(|| {
        let mut clipboard = Clipboard::new()
            .map_err(|e| Error::wrap(e, ErrorKind::Read)
                .with_msg("clipboard: Failed to initialize clipboard")
            )?;

        let text = clipboard.get_text()
            .map_err(|e| Error::wrap(e, ErrorKind::Read)
                .with_msg("clipboard: Failed to read clipboard text")
            )?;

        Ok(text)
    })
    .await
    .map_err(|e| Error::wrap(e, ErrorKind::Read)
        .with_msg("clipboard: Task join failed")
    )?
}

pub fn set_clipboard(text: String) {
    tokio::task::spawn_blocking(move || {
        let mut clipboard = match Clipboard::new() {
            Ok(c) => c,
            Err(e) => {
                let e = Error::wrap(e, ErrorKind::Write)
                    .with_msg("clipboard: Failed to initialize clipboard");
                error(&e);
                return;
            }
        };

        if let Err(e) = clipboard.set()
            .wait()
            .clipboard(LinuxClipboardKind::Clipboard)
            .text(text)
        {
            let e = Error::wrap(e, ErrorKind::Write)
                .with_msg("clipboard: Failed to write clipboard text");
            error(&e);
        }
    });
}
