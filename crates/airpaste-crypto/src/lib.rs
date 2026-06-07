//! End-to-end encryption for Air Paste clip content.
//!
//! Each clip gets a random 32-byte content key (CEK). The body is encrypted with
//! XChaCha20-Poly1305 under the CEK. The CEK is then sealed to each authorized
//! recipient device: an ephemeral X25519 key agreement with the recipient's static
//! public key produces a shared secret, HKDF-SHA256 derives a wrapping key bound to
//! both public keys, and the CEK is AEAD-encrypted under that wrapping key.
//!
//! The server only ever sees ciphertext, ephemeral public keys, and nonces.

use airpaste_core::{DeviceId, WrappedKey};
use base64::{engine::general_purpose::STANDARD, Engine as _};
use chacha20poly1305::{
    aead::{Aead, KeyInit, Payload},
    XChaCha20Poly1305, XNonce,
};
use hkdf::Hkdf;
use rand_core::{OsRng, RngCore};
use sha2::Sha256;
use x25519_dalek::{EphemeralSecret, PublicKey, StaticSecret};

/// Scheme identifier stored in `EncryptionInfo.scheme` for encrypted text clips.
pub const TEXT_ENCRYPTION_SCHEME: &str = "airpaste-x25519-xchacha20poly1305-v1";

const KEY_WRAP_INFO: &[u8] = b"airpaste-key-wrap-v1";
const BODY_AAD: &[u8] = b"airpaste-text-body-v1";
const NONCE_LEN: usize = 24;
const KEY_LEN: usize = 32;

#[derive(Debug, thiserror::Error)]
pub enum CryptoError {
    #[error("base64 decode failed: {0}")]
    Base64(#[from] base64::DecodeError),
    #[error("invalid key or nonce length")]
    Length,
    #[error("AEAD encrypt/decrypt failed")]
    Aead,
    #[error("HKDF expansion failed")]
    Hkdf,
    #[error("no wrapped content key for this device")]
    NoWrappedKey,
    #[error("decrypted text was not valid UTF-8")]
    Utf8,
}

/// A device's long-term X25519 key-agreement identity, separate from its Ed25519
/// signing identity.
pub struct EncryptionIdentity {
    secret: StaticSecret,
}

impl EncryptionIdentity {
    pub fn generate() -> Self {
        Self {
            secret: StaticSecret::random_from_rng(OsRng),
        }
    }

    pub fn from_private_key_base64(value: &str) -> Result<Self, CryptoError> {
        let key = decode_array::<KEY_LEN>(value)?;
        Ok(Self {
            secret: StaticSecret::from(key),
        })
    }

    pub fn private_key_base64(&self) -> String {
        STANDARD.encode(self.secret.to_bytes())
    }

    pub fn public_key_base64(&self) -> String {
        STANDARD.encode(PublicKey::from(&self.secret).to_bytes())
    }
}

/// A device authorized to decrypt a clip, identified by its X25519 public key.
pub struct Recipient {
    pub device_id: DeviceId,
    pub public_key_base64: String,
}

/// Output of [`seal_text`]: the encrypted body plus per-recipient wrapped keys.
pub struct SealedText {
    pub body_ciphertext_base64: String,
    pub body_nonce_base64: String,
    pub wrapped_keys: Vec<WrappedKey>,
}

/// Encrypt `plaintext` for every recipient. Returns an error if `recipients` is empty.
pub fn seal_text(plaintext: &str, recipients: &[Recipient]) -> Result<SealedText, CryptoError> {
    if recipients.is_empty() {
        return Err(CryptoError::NoWrappedKey);
    }

    let mut cek = [0u8; KEY_LEN];
    OsRng.fill_bytes(&mut cek);
    let mut body_nonce = [0u8; NONCE_LEN];
    OsRng.fill_bytes(&mut body_nonce);

    let body_cipher = XChaCha20Poly1305::new_from_slice(&cek).map_err(|_| CryptoError::Length)?;
    let body_ciphertext = body_cipher
        .encrypt(
            XNonce::from_slice(&body_nonce),
            Payload {
                msg: plaintext.as_bytes(),
                aad: BODY_AAD,
            },
        )
        .map_err(|_| CryptoError::Aead)?;

    let mut wrapped_keys = Vec::with_capacity(recipients.len());
    for recipient in recipients {
        let recipient_pub = decode_public_key(&recipient.public_key_base64)?;
        let ephemeral = EphemeralSecret::random_from_rng(OsRng);
        let ephemeral_pub = PublicKey::from(&ephemeral);
        let shared = ephemeral.diffie_hellman(&recipient_pub);
        let wrap_key = derive_wrap_key(
            shared.as_bytes(),
            ephemeral_pub.as_bytes(),
            recipient_pub.as_bytes(),
        )?;

        let mut wrap_nonce = [0u8; NONCE_LEN];
        OsRng.fill_bytes(&mut wrap_nonce);
        let wrap_cipher =
            XChaCha20Poly1305::new_from_slice(&wrap_key).map_err(|_| CryptoError::Length)?;
        let wrapped = wrap_cipher
            .encrypt(XNonce::from_slice(&wrap_nonce), cek.as_ref())
            .map_err(|_| CryptoError::Aead)?;

        wrapped_keys.push(WrappedKey {
            device_id: recipient.device_id.clone(),
            ephemeral_public_key: STANDARD.encode(ephemeral_pub.to_bytes()),
            nonce: STANDARD.encode(wrap_nonce),
            ciphertext: STANDARD.encode(wrapped),
        });
    }

    Ok(SealedText {
        body_ciphertext_base64: STANDARD.encode(body_ciphertext),
        body_nonce_base64: STANDARD.encode(body_nonce),
        wrapped_keys,
    })
}

/// Decrypt a sealed text body using this device's wrapped key.
pub fn open_text(
    body_ciphertext_base64: &str,
    body_nonce_base64: &str,
    wrapped_keys: &[WrappedKey],
    device_id: &DeviceId,
    identity: &EncryptionIdentity,
) -> Result<String, CryptoError> {
    let wrapped = wrapped_keys
        .iter()
        .find(|key| &key.device_id == device_id)
        .ok_or(CryptoError::NoWrappedKey)?;

    let ephemeral_pub = decode_public_key(&wrapped.ephemeral_public_key)?;
    let shared = identity.secret.diffie_hellman(&ephemeral_pub);
    let recipient_pub = PublicKey::from(&identity.secret);
    let wrap_key = derive_wrap_key(
        shared.as_bytes(),
        ephemeral_pub.as_bytes(),
        recipient_pub.as_bytes(),
    )?;

    let wrap_nonce = decode_array::<NONCE_LEN>(&wrapped.nonce)?;
    let wrapped_ct = STANDARD.decode(&wrapped.ciphertext)?;
    let wrap_cipher =
        XChaCha20Poly1305::new_from_slice(&wrap_key).map_err(|_| CryptoError::Length)?;
    let cek = wrap_cipher
        .decrypt(XNonce::from_slice(&wrap_nonce), wrapped_ct.as_ref())
        .map_err(|_| CryptoError::Aead)?;
    let cek: [u8; KEY_LEN] = cek.as_slice().try_into().map_err(|_| CryptoError::Length)?;

    let body_nonce = decode_array::<NONCE_LEN>(body_nonce_base64)?;
    let body_ct = STANDARD.decode(body_ciphertext_base64)?;
    let body_cipher = XChaCha20Poly1305::new_from_slice(&cek).map_err(|_| CryptoError::Length)?;
    let plaintext = body_cipher
        .decrypt(
            XNonce::from_slice(&body_nonce),
            Payload {
                msg: &body_ct,
                aad: BODY_AAD,
            },
        )
        .map_err(|_| CryptoError::Aead)?;

    String::from_utf8(plaintext).map_err(|_| CryptoError::Utf8)
}

fn derive_wrap_key(
    shared: &[u8],
    ephemeral_pub: &[u8; KEY_LEN],
    recipient_pub: &[u8; KEY_LEN],
) -> Result<[u8; KEY_LEN], CryptoError> {
    let hkdf = Hkdf::<Sha256>::new(None, shared);
    let mut info = Vec::with_capacity(KEY_WRAP_INFO.len() + 2 * KEY_LEN);
    info.extend_from_slice(KEY_WRAP_INFO);
    info.extend_from_slice(ephemeral_pub);
    info.extend_from_slice(recipient_pub);
    let mut okm = [0u8; KEY_LEN];
    hkdf.expand(&info, &mut okm).map_err(|_| CryptoError::Hkdf)?;
    Ok(okm)
}

fn decode_public_key(value: &str) -> Result<PublicKey, CryptoError> {
    Ok(PublicKey::from(decode_array::<KEY_LEN>(value)?))
}

fn decode_array<const N: usize>(value: &str) -> Result<[u8; N], CryptoError> {
    let bytes = STANDARD.decode(value)?;
    bytes.as_slice().try_into().map_err(|_| CryptoError::Length)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn recipient(identity: &EncryptionIdentity, id: &str) -> Recipient {
        Recipient {
            device_id: DeviceId(id.to_string()),
            public_key_base64: identity.public_key_base64(),
        }
    }

    #[test]
    fn round_trips_for_authorized_recipient() {
        let alice = EncryptionIdentity::generate();
        let bob = EncryptionIdentity::generate();
        let recipients = vec![recipient(&alice, "alice"), recipient(&bob, "bob")];

        let sealed = seal_text("top secret \u{1f510}", &recipients).unwrap();
        // Server stores only ciphertext; it never matches the plaintext.
        assert!(!sealed.body_ciphertext_base64.contains("secret"));

        let opened = open_text(
            &sealed.body_ciphertext_base64,
            &sealed.body_nonce_base64,
            &sealed.wrapped_keys,
            &DeviceId("bob".to_string()),
            &bob,
        )
        .unwrap();
        assert_eq!(opened, "top secret \u{1f510}");
    }

    #[test]
    fn persisted_identity_can_decrypt() {
        let bob = EncryptionIdentity::generate();
        let restored = EncryptionIdentity::from_private_key_base64(&bob.private_key_base64()).unwrap();
        let sealed = seal_text("hello", &[recipient(&bob, "bob")]).unwrap();
        let opened = open_text(
            &sealed.body_ciphertext_base64,
            &sealed.body_nonce_base64,
            &sealed.wrapped_keys,
            &DeviceId("bob".to_string()),
            &restored,
        )
        .unwrap();
        assert_eq!(opened, "hello");
    }

    #[test]
    fn wrong_device_cannot_decrypt() {
        let bob = EncryptionIdentity::generate();
        let eve = EncryptionIdentity::generate();
        let sealed = seal_text("hello", &[recipient(&bob, "bob")]).unwrap();

        // Eve is not in the wrapped-key list at all.
        let missing = open_text(
            &sealed.body_ciphertext_base64,
            &sealed.body_nonce_base64,
            &sealed.wrapped_keys,
            &DeviceId("eve".to_string()),
            &eve,
        );
        assert!(matches!(missing, Err(CryptoError::NoWrappedKey)));

        // Even if Eve forges bob's device id onto her key lookup, her secret can't unwrap.
        let forged = open_text(
            &sealed.body_ciphertext_base64,
            &sealed.body_nonce_base64,
            &sealed.wrapped_keys,
            &DeviceId("bob".to_string()),
            &eve,
        );
        assert!(matches!(forged, Err(CryptoError::Aead)));
    }
}
