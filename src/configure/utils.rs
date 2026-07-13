use std::ffi::CStr;
use std::io::{self, Write};
use std::sync::atomic::{AtomicBool, Ordering};

use crate::errors::{Error, ErrorKind, Result};

pub(crate) static INTERRUPTED: AtomicBool = AtomicBool::new(false);

pub(crate) extern "C" fn handle_interrupt(_sig: libc::c_int) {
    INTERRUPTED.store(true, Ordering::SeqCst);
}

pub(crate) fn interrupted() -> bool {
    INTERRUPTED.load(Ordering::SeqCst)
}

pub(crate) fn is_root() -> bool {
    unsafe { libc::geteuid() == 0 }
}

pub(crate) fn ensure_port(address: &str, port: u16) -> String {
    if let Some(host) = address.strip_prefix('[') {
        if host.contains("]:") {
            return address.to_string();
        }
        return format!("{}:{}", address, port);
    }

    match address.matches(':').count() {
        0 => format!("{}:{}", address, port),
        1 => address.to_string(),
        _ => format!("[{}]:{}", address, port),
    }
}

pub(crate) fn print_host_prompt() -> Result<()> {
    print!("Enter host address to add manually, press Ctrl+C to finish: ");
    io::stdout().flush().map_err(|e| {
        Error::wrap(e, ErrorKind::Write)
            .with_msg("configure: Failed to flush stdout")
    })
}

pub(crate) fn get_hostname() -> Result<String> {
    let mut buf = [0 as libc::c_char; 256];
    let ret = unsafe { libc::gethostname(buf.as_mut_ptr(), buf.len()) };
    if ret != 0 {
        return Err(Error::wrap(io::Error::last_os_error(), ErrorKind::Read)
            .with_msg("configure: Failed to get hostname"));
    }
    buf[buf.len() - 1] = 0;

    let hostname = unsafe { CStr::from_ptr(buf.as_ptr()) };
    Ok(hostname.to_string_lossy().into_owned())
}
