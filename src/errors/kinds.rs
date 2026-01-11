use std::fmt;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum ErrorKind {
    Read,
    Write,
    Parse,
    Invalid,
    Serialization,
    NotFound,
    Conflict,
    Network,
    Connection,
    Timeout,
    Request,
    Unauthorized,
    Database,
    Exec,
    Cancelled,
    Unknown,
}

impl fmt::Display for ErrorKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:?}", self)
    }
}
