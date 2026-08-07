use chacha20poly1305::{
    XChaCha20Poly1305, XNonce,
    aead::{Aead, KeyInit, Payload},
};
use rand::{RngCore, rngs::OsRng};
use uuid::Uuid;

#[derive(Debug, thiserror::Error)]
pub enum CryptoError {
    #[error("encryption failed")]
    Encrypt,
    #[error("decryption failed")]
    Decrypt,
    #[error("malformed nonce")]
    BadNonce,
}

/// Encrypts `plaintext` under `master_key`, binding the result to `cred_id` and `user_id`
/// so ciphertext cannot be swapped between rows.
///
/// Returns `(ciphertext_with_tag, nonce)`. The nonce is 24 bytes (XChaCha20), CSPRNG-generated.
/// The ciphertext has the 16-byte Poly1305 tag appended by the underlying crate.
pub fn encrypt_key(
    plaintext: &[u8],
    master_key: &[u8; 32],
    cred_id: Uuid,
    user_id: Uuid,
) -> Result<(Vec<u8>, Vec<u8>), CryptoError> {
    let cipher = XChaCha20Poly1305::new(master_key.into());

    let mut nonce_bytes = [0u8; 24];
    OsRng.fill_bytes(&mut nonce_bytes);
    let nonce = XNonce::from(nonce_bytes);

    let aad = format!("credential:{cred_id}:{user_id}");
    let ciphertext = cipher
        .encrypt(&nonce, Payload { msg: plaintext, aad: aad.as_bytes() })
        .map_err(|_| CryptoError::Encrypt)?;

    Ok((ciphertext, nonce_bytes.to_vec()))
}

/// Decrypts `ciphertext` (ciphertext‖tag) under `master_key`, verifying the row-binding AAD.
pub fn decrypt_key(
    ciphertext: &[u8],
    nonce_bytes: &[u8],
    master_key: &[u8; 32],
    cred_id: Uuid,
    user_id: Uuid,
) -> Result<Vec<u8>, CryptoError> {
    let cipher = XChaCha20Poly1305::new(master_key.into());

    let nonce_arr: [u8; 24] = nonce_bytes.try_into().map_err(|_| CryptoError::BadNonce)?;
    let nonce = XNonce::from(nonce_arr);

    let aad = format!("credential:{cred_id}:{user_id}");
    cipher
        .decrypt(&nonce, Payload { msg: ciphertext, aad: aad.as_bytes() })
        .map_err(|_| CryptoError::Decrypt)
}
