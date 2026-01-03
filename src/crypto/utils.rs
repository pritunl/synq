use base64::{Engine as _, engine::general_purpose::STANDARD_NO_PAD};
use crypto_box::{
    aead::{Aead, AeadCore, OsRng},
    PublicKey, SecretKey,
};

use crate::errors::{Result, Error, ErrorKind};

pub(super) fn decode_secret_key(b64: &str) -> Result<SecretKey> {
    let bytes = STANDARD_NO_PAD
        .decode(b64)
        .map_err(|e| Error::wrap(e, ErrorKind::Parse)
        .with_msg("crypto: invalid base64 private key"))?;

    let bytes: [u8; 32] = bytes
        .try_into()
        .map_err(|_| Error::new(ErrorKind::Invalid)
        .with_msg("crypto: private key must be 32 bytes"))?;

    Ok(SecretKey::from(bytes))
}

pub(super) fn decode_public_key(b64: &str) -> Result<PublicKey> {
    let bytes = STANDARD_NO_PAD
        .decode(b64)
        .map_err(|e| Error::wrap(e, ErrorKind::Parse)
        .with_msg("crypto: invalid base64 public key"))?;

    let bytes: [u8; 32] = bytes
        .try_into()
        .map_err(|_| Error::new(ErrorKind::Invalid)
        .with_msg("crypto: public key must be 32 bytes"))?;

    Ok(PublicKey::from(bytes))
}

pub fn secret_key_to_public_key(b64: &str) -> Result<String> {
    let bytes = STANDARD_NO_PAD
        .decode(b64)
        .map_err(|e| Error::wrap(e, ErrorKind::Parse)
        .with_msg("crypto: invalid base64 private key"))?;

    let bytes: [u8; 32] = bytes
        .try_into()
        .map_err(|_| Error::new(ErrorKind::Invalid)
        .with_msg("crypto: private key must be 32 bytes"))?;

    let pub64 = STANDARD_NO_PAD.encode(SecretKey::from(bytes).public_key().to_bytes());

    Ok(pub64)
}

pub fn generate_keypair() -> (String, String) {
    let secret_key = SecretKey::generate(&mut OsRng);
    let public_key = secret_key.public_key();
    (
        STANDARD_NO_PAD.encode(secret_key.to_bytes()),
        STANDARD_NO_PAD.encode(public_key.to_bytes()),
    )
}
