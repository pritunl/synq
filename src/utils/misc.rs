use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::LazyLock;
use std::time::{Duration, Instant};

use crate::errors::{Result, Error, ErrorKind};

static START_TIME: std::sync::OnceLock<Instant> = std::sync::OnceLock::new();
static SAFE_CHARS: LazyLock<HashSet<char>> = LazyLock::new(|| {
    HashSet::from([
        'a', 'b', 'c', 'd', 'e', 'f', 'g', 'h', 'i', 'j', 'k', 'l', 'm',
        'n', 'o', 'p', 'q', 'r', 's', 't', 'u', 'v', 'w', 'x', 'y', 'z',
        'A', 'B', 'C', 'D', 'E', 'F', 'G', 'H', 'I', 'J', 'K', 'L', 'M',
        'N', 'O', 'P', 'Q', 'R', 'S', 'T', 'U', 'V', 'W', 'X', 'Y', 'Z',
        '0', '1', '2', '3', '4', '5', '6', '7', '8', '9',
        '-', '+', '=', '_', '/', ',', '.', '~', '@', '#', '!', '&', ' ',
    ])
});

pub fn filter_str(s: &str, n: usize) -> String {
    s.chars()
        .take(n)
        .filter(|c| SAFE_CHARS.contains(c))
        .collect()
}

pub fn mono_time_ms() -> u64 {
    let start = START_TIME.get_or_init(Instant::now);
    start.elapsed().as_millis() as u64
}

pub fn get_config_path() -> Result<PathBuf> {
    let home = std::env::var("HOME")
        .map_err(|e| Error::wrap(e, ErrorKind::Parse)
            .with_msg("utils: Failed to get home environment variable"))?;

    Ok(PathBuf::from(home).join(".config/synq.conf"))
}
