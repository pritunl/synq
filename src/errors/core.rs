use std::fmt;
use std::collections::HashMap;
use backtrace::Backtrace;

use super::ErrorKind;

pub type Result<T, E = Error> = std::result::Result<T, E>;

pub struct Error {
    kind: ErrorKind,
    message: String,
    source: Option<Box<dyn std::error::Error + Send + Sync>>,
    backtrace: Backtrace,
    context: HashMap<String, String>,
}

impl Error {
    pub fn new(kind: ErrorKind) -> Self {
        Self {
            kind,
            message: String::new(),
            source: None,
            backtrace: Backtrace::new(),
            context: HashMap::new(),
        }
    }

    pub fn with_msg(mut self, message: impl Into<String>) -> Self {
        self.message = message.into();
        self
    }

    pub fn newf(kind: ErrorKind, args: fmt::Arguments) -> Self {
        Self {
            kind,
            message: args.to_string(),
            source: None,
            backtrace: Backtrace::new(),
            context: HashMap::new(),
        }
    }

    pub fn wrap(source: impl std::error::Error + Send + Sync + 'static, kind: ErrorKind) -> Self {
        Self {
            kind,
            message: String::new(),
            source: Some(Box::new(source)),
            backtrace: Backtrace::new(),
            context: HashMap::new(),
        }
    }

    pub fn wrapf(source: impl std::error::Error + Send + Sync + 'static, kind: ErrorKind, args: fmt::Arguments) -> Self {
        Self {
            kind,
            message: args.to_string(),
            source: Some(Box::new(source)),
            backtrace: Backtrace::new(),
            context: HashMap::new(),
        }
    }

    pub fn with_ctx(mut self, key: impl Into<String>, value: impl ToString) -> Self {
        self.context.insert(key.into(), value.to_string());
        self
    }

    pub fn set_ctx(&mut self, key: impl Into<String>, value: impl ToString) {
        self.context.insert(key.into(), value.to_string());
    }

    pub fn get_ctx(&self, key: &str) -> Option<&str> {
        self.context.get(key).map(|s| s.as_str())
    }

    pub fn ctx(&self) -> &HashMap<String, String> {
        &self.context
    }

    pub fn kind(&self) -> &ErrorKind {
        &self.kind
    }

    pub fn is_kind(&self, kind: ErrorKind) -> bool {
        self.kind == kind
    }

    pub fn msg(&self) -> &str {
        &self.message
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "[{}]", self.kind)?;

        match (&self.message.is_empty(), &self.source) {
            (false, Some(source)) => write!(f, " {} {}", self.message, source)?,
            (false, None) => write!(f, " {}", self.message)?,
            (true, Some(source)) => write!(f, " {}", source)?,
            (true, None) => {},
        }

        Ok(())
    }
}

impl fmt::Debug for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.message.is_empty() {
            writeln!(f, "[{}]", self.kind)?;
        } else {
            writeln!(f, "[{}] {}", self.kind, self.message)?;
        }

        if !self.context.is_empty() {
            writeln!(f, "Context:")?;
            for (key, value) in &self.context {
                writeln!(f, "  {}: {}", key, value)?;
            }
        }

        if let Some(source) = &self.source {
            writeln!(f, "Source: {:?}", source)?;
        }

        writeln!(f, "Backtrace:")?;
        write!(f, "{:?}", self.backtrace)
    }
}

impl From<Error> for String {
    fn from(err: Error) -> String {
        if err.message.is_empty() {
            format!("[{}]", err.kind)
        } else {
            format!("[{}] {}", err.kind, err.message)
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        self.source.as_ref().map(|e| e.as_ref() as _)
    }
}
