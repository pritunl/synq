use arboard::Clipboard;

use crate::errors::{Result, Error, ErrorKind};

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
