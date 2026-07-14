use std::io::{self, Write};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::thread;
use std::time::Duration;

use crate::errors::error;
use crate::errors::{Error, ErrorKind, Result};
use super::constants::HOST_POLL_INTERVAL;
use super::utils::interrupted;

pub(crate) struct Prompt {
    pub(crate) lines: Receiver<String>,
}

impl Prompt {
    pub(crate) fn start() -> Self {
        let (tx, rx) = mpsc::channel();

        thread::spawn(move || {
            loop {
                let mut line = String::new();
                match io::stdin().read_line(&mut line) {
                    Ok(0) => break,
                    Ok(_) => {
                        if tx.send(line.trim().to_string()).is_err() {
                            break;
                        }
                    }
                    Err(e) => {
                        let e = Error::wrap(e, ErrorKind::Read)
                            .with_msg("configure: Failed to read input");
                        error(&e);
                        break;
                    }
                }
            }
        });

        Self { lines: rx }
    }

    pub(crate) fn line(&self, prompt: &str) -> Result<String> {
        print!("{}", prompt);
        io::stdout().flush().map_err(|e| {
            Error::wrap(e, ErrorKind::Write)
                .with_msg("configure: Failed to flush stdout")
        })?;

        loop {
            if interrupted() {
                return Err(Error::new(ErrorKind::Cancelled)
                    .with_msg("configure: Interrupted"));
            }

            match self.lines
                .recv_timeout(Duration::from_millis(HOST_POLL_INTERVAL))
            {
                Ok(line) => return Ok(line),
                Err(RecvTimeoutError::Timeout) => continue,
                Err(RecvTimeoutError::Disconnected) => {
                    return Err(Error::new(ErrorKind::Read)
                        .with_msg("configure: Unexpected end of input"));
                }
            }
        }
    }

    pub(crate) fn line_default(&self, prompt: &str, default: &str) -> Result<String> {
        let line = self.line(&format!("{} [{}]: ", prompt, default))?;
        if line.is_empty() {
            Ok(default.to_string())
        } else {
            Ok(line)
        }
    }

    pub(crate) fn yes_no_default(&self, prompt: &str, default: bool) -> Result<bool> {
        let hint = if default { "Y/n" } else { "y/N" };
        loop {
            let line = self.line(&format!("{} [{}]: ", prompt, hint))?
                .to_lowercase();
            match line.as_str() {
                "" => return Ok(default),
                "y" | "yes" => return Ok(true),
                "n" | "no" => return Ok(false),
                _ => println!("Enter y or n"),
            }
        }
    }
}
