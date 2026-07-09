use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use tokio::fs;
use serde_saphyr::{from_str, to_string};

use crate::errors::{Result, Error, ErrorKind};
use crate::crypto::{generate_keypair, secret_key_to_public_key};
use super::constants::DEFAULT_BIND;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    #[serde(skip)]
    path: PathBuf,
    #[serde(skip)]
    modified: bool,
    pub server: ServerConfig,
    pub peers: Vec<PeerConfig>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServerConfig {
    pub bind: String,
    #[serde(default)]
    pub address: String,
    #[serde(default)]
    pub private_key: String,
    #[serde(default)]
    pub public_key: String,
    #[serde(default)]
    pub clipboard_source: bool,
    #[serde(default)]
    pub clipboard_destination: bool,
    #[serde(default)]
    pub scroll_source: bool,
    #[serde(default)]
    pub scroll_destination: bool,
    #[serde(default)]
    pub scroll_input_devices: Vec<InputDevice>,
}

impl ServerConfig {
    pub fn bind_port(&self) -> Result<u16> {
        self.bind
            .rsplit_once(':')
            .and_then(|(_, port)| port.parse::<u16>().ok())
            .ok_or_else(|| Error::new(ErrorKind::Parse)
                .with_msg("config: Failed to parse bind port")
                .with_ctx("bind", self.bind.clone())
            )
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PeerConfig {
    pub address: String,
    pub public_key: String,
    #[serde(default)]
    pub clipboard_source: bool,
    #[serde(default)]
    pub clipboard_destination: bool,
    #[serde(default)]
    pub scroll_source: bool,
    #[serde(default)]
    pub scroll_destination: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InputDevice {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    #[serde(default = "default_scroll_reverse")]
    pub scroll_reverse: bool,
    #[serde(default = "default_scroll_modifier")]
    pub scroll_modifier: f64,
}

impl Default for InputDevice {
    fn default() -> Self {
        Self {
            name: None,
            path: None,
            scroll_reverse: default_scroll_reverse(),
            scroll_modifier: default_scroll_modifier(),
        }
    }
}

const fn default_scroll_reverse() -> bool {
    true
}

const fn default_scroll_modifier() -> f64 {
    1.0
}

impl Config {
    pub async fn load<P: AsRef<Path>>(path: P) -> Result<Self> {
        let path = path.as_ref();

        let contents = fs::read_to_string(path)
            .await
            .map_err(|e| Error::wrap(e, ErrorKind::Read)
                .with_msg("config: Failed to read file")
                .with_ctx("path", path.display().to_string())
            )?;

        let mut config: Config = from_str(&contents)
            .map_err(|e| Error::wrap(e, ErrorKind::Parse)
                .with_msg("config: Failed to parse")
                .with_ctx("path", path.display().to_string())
            )?;

        config.path = path.to_path_buf();
        config.normalize()?;
        config.validate()?;

        Ok(config)
    }

    pub async fn load_or_create<P: AsRef<Path>>(path: P) -> Result<Self> {
        let path = path.as_ref();

        let exists = fs::try_exists(path)
            .await
            .map_err(|e| Error::wrap(e, ErrorKind::Read)
                .with_msg("config: Failed to check file")
                .with_ctx("path", path.display().to_string())
            )?;
        if exists {
            return Self::load(path).await;
        }

        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)
                .await
                .map_err(|e| Error::wrap(e, ErrorKind::Write)
                    .with_msg("config: Failed to create directory")
                    .with_ctx("path", parent.display().to_string())
                )?;
        }

        let mut config = Config {
            path: path.to_path_buf(),
            modified: true,
            server: ServerConfig {
                bind: DEFAULT_BIND.to_string(),
                address: String::new(),
                private_key: String::new(),
                public_key: String::new(),
                clipboard_source: false,
                clipboard_destination: false,
                scroll_source: false,
                scroll_destination: false,
                scroll_input_devices: Vec::new(),
            },
            peers: Vec::new(),
        };
        config.normalize()?;

        Ok(config)
    }

    pub async fn save(&self) -> Result<()> {
        self.validate()?;

        let contents = to_string(self)
            .map_err(|e| Error::wrap(e, ErrorKind::Write)
                .with_msg("config: Failed to serialize")
            )?;

        fs::write(&self.path, &contents)
            .await
            .map_err(|e| Error::wrap(e, ErrorKind::Write)
                .with_msg("config: Failed to write file")
                .with_ctx("path", self.path.display().to_string())
            )?;

        Ok(())
    }

    pub fn is_modified(&self) -> bool {
        self.modified
    }

    pub fn set_keypair(&mut self, private_key: String, public_key: String) {
        self.server.private_key = private_key;
        self.server.public_key = public_key;
    }

    fn normalize(&mut self) -> Result<()> {
        if self.server.private_key.is_empty() {
            let (secret, public) = generate_keypair();
            self.server.private_key = secret;
            self.server.public_key = public;
            self.modified = true;
        } else if self.server.public_key.is_empty() {
            self.server.public_key = secret_key_to_public_key(&self.server.private_key)?;
            self.modified = true;
        }
        Ok(())
    }

    fn validate(&self) -> Result<()> {
        if self.server.bind.is_empty() {
            return Err(Error::new(ErrorKind::Parse)
                .with_msg("config: Server bind address cannot be empty"));
        }

        if self.server.private_key.is_empty() {
            return Err(Error::new(ErrorKind::Parse)
                .with_msg("config: Private key cannot be empty"));
        }

        if self.server.public_key.is_empty() {
            return Err(Error::new(ErrorKind::Parse)
                .with_msg("config: Public key cannot be empty"));
        }

        for (i, peer) in self.peers.iter().enumerate() {
            if peer.address.is_empty() {
                return Err(Error::new(ErrorKind::Parse)
                    .with_msg("config: Peer address cannot be empty")
                    .with_ctx("peer_index", i));
            }

            if peer.public_key.is_empty() {
                return Err(Error::new(ErrorKind::Parse)
                    .with_msg("config: Peer public key cannot be empty")
                    .with_ctx("peer_index", i));
            }
        }

        Ok(())
    }
}
