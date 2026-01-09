use std::collections::HashMap;
use std::sync::{Arc, RwLock};

use crypto_box::{SalsaBox, SecretKey};

use crate::errors::{Result, Error, ErrorKind};
use super::utils::{decode_public_key, decode_secret_key};

pub struct KeyStore {
    host_secret_key: SecretKey,
    boxes: RwLock<HashMap<String, Arc<SalsaBox>>>,
}

impl KeyStore {
    pub fn new(host_private_key_b64: &str) -> Result<Self> {
        let host_secret_key = decode_secret_key(host_private_key_b64)?;
        Ok(Self {
            host_secret_key,
            boxes: RwLock::new(HashMap::new()),
        })
    }

    pub fn get_box(&self, client_public_key_b64: &str) -> Result<Arc<SalsaBox>> {
        {
            let boxes = self.boxes.read().map_err(|_| {
                Error::new(ErrorKind::Exec).with_msg("crypto: lock poisoned")
            })?;
            if let Some(salsa_box) = boxes.get(client_public_key_b64) {
                return Ok(Arc::clone(salsa_box));
            }
        }

        let client_public_key = decode_public_key(client_public_key_b64)?;

        let mut boxes = self.boxes.write().map_err(|_| {
            Error::new(ErrorKind::Exec).with_msg("crypto: lock poisoned")
        })?;

        if let Some(salsa_box) = boxes.get(client_public_key_b64) {
            return Ok(Arc::clone(salsa_box));
        }

        let salsa_box = Arc::new(SalsaBox::new(&client_public_key, &self.host_secret_key));
        boxes.insert(client_public_key_b64.to_owned(), Arc::clone(&salsa_box));
        Ok(salsa_box)
    }

    #[allow(unused)]
    pub fn remove(&self, client_public_key_b64: &str) -> Result<bool> {
        let mut boxes = self.boxes.write().map_err(|_| {
            Error::new(ErrorKind::Exec).with_msg("crypto: lock poisoned")
        })?;
        Ok(boxes.remove(client_public_key_b64).is_some())
    }

    #[allow(unused)]
    pub fn clear(&self) -> Result<()> {
        let mut boxes = self.boxes.write().map_err(|_| {
            Error::new(ErrorKind::Exec).with_msg("crypto: lock poisoned")
        })?;
        boxes.clear();
        Ok(())
    }
}
