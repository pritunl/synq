use super::{Error, ErrorKind};
use std::sync::LazyLock;
use std::sync::RwLock;
use std::time::{Duration, Instant};
use std::collections::HashMap;

pub use tracing::{trace, debug, info, warn};

const RATE_LIMIT_MAX_ERRORS: usize = 5;
const RATE_LIMIT_WINDOW_SECS: u64 = 60;

static ERROR_RATE_LIMITER: LazyLock<RwLock<HashMap<ErrorKind, RateLimitState>>> =
    LazyLock::new(|| RwLock::new(HashMap::new()));

#[derive(Clone)]
struct RateLimitState {
    count: usize,
    window_start: Instant,
}

impl RateLimitState {
    fn new() -> Self {
        Self {
            count: 1,
            window_start: Instant::now(),
        }
    }

    fn should_log(&mut self) -> bool {
        let now = Instant::now();
        let window_duration = Duration::from_secs(RATE_LIMIT_WINDOW_SECS);

        if now.duration_since(self.window_start) >= window_duration {
            self.count = 1;
            self.window_start = now;
            return true;
        }

        if self.count < RATE_LIMIT_MAX_ERRORS {
            self.count += 1;
            return true;
        }

        false
    }
}

pub fn error(e: &Error) {
    let kind = e.kind().clone();

    let should_log = {
        let mut limiter = ERROR_RATE_LIMITER.write().unwrap();
        let state = limiter.entry(kind).or_insert_with(RateLimitState::new);
        state.should_log()
    };

    if should_log {
        tracing::error!(
            error_kind = %e.kind(),
            error_msg = %e.msg(),
            "{:?}", e
        );
    } else {
        tracing::trace!(
            error_kind = %e.kind(),
            "Rate limiting error logs"
        );
    }
}
