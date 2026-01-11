use super::Error;

pub use tracing::{trace, debug, info, warn};

pub fn error(e: &Error) {
    tracing::error!(
        error_kind = %e.kind(),
        error_msg = %e.msg(),
        "{:?}", e
    );
}
