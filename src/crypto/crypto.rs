use base64::{Engine as _, engine::general_purpose::STANDARD_NO_PAD};
use crypto_box::{
    aead::{Aead, AeadCore, OsRng},
    SalsaBox, PublicKey, SecretKey, Nonce,
};

use crate::errors::{Result, Error, ErrorKind};
use super::utils::{decode_public_key, decode_secret_key};

const NONCE_SIZE: usize = 24;

pub fn encrypt(
    host_private_key_b64: &str,
    client_public_key_b64: &str,
    plaintext: &str,
) -> Result<String> {
    let host_secret_key = decode_secret_key(host_private_key_b64)?;
    let client_public_key = decode_public_key(client_public_key_b64)?;

    let salsa_box = SalsaBox::new(&client_public_key, &host_secret_key);

    let nonce = SalsaBox::generate_nonce(&mut OsRng);

    let ciphertext = salsa_box
        .encrypt(&nonce, plaintext.as_bytes())
        .map_err(|_| Error::new(ErrorKind::Exec)
        .with_msg("crypto: encryption failed"))?;

    let mut combined = Vec::with_capacity(NONCE_SIZE + ciphertext.len());
    combined.extend_from_slice(nonce.as_slice());
    combined.extend_from_slice(&ciphertext);

    Ok(STANDARD_NO_PAD.encode(&combined))
}

pub fn decrypt(
    host_private_key_b64: &str,
    client_public_key_b64: &str,
    ciphertext_b64: &str,
) -> Result<String> {
    let host_secret_key = decode_secret_key(host_private_key_b64)?;
    let client_public_key = decode_public_key(client_public_key_b64)?;

    let combined = STANDARD_NO_PAD
        .decode(ciphertext_b64)
        .map_err(|e| Error::wrap(e, ErrorKind::Parse)
        .with_msg("crypto: invalid base64 ciphertext"))?;

    if combined.len() < NONCE_SIZE {
        return Err(Error::new(ErrorKind::Invalid)
            .with_msg("crypto: ciphertext too short"));
    }

    let (nonce_bytes, ciphertext) = combined.split_at(NONCE_SIZE);
    let nonce = Nonce::from_slice(nonce_bytes);

    let salsa_box = SalsaBox::new(&client_public_key, &host_secret_key);

    let plaintext_bytes = salsa_box
        .decrypt(nonce, ciphertext)
        .map_err(|_| Error::new(ErrorKind::Exec)
        .with_msg("crypto: decryption failed"))?;

    String::from_utf8(plaintext_bytes)
        .map_err(|e| Error::wrap(e, ErrorKind::Parse)
        .with_msg("crypto: invalid UTF-8 in decrypted data"))
}
