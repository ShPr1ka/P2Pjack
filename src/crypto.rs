//! Шифрование трафика сети.
//!
//! Из имени сети и пароля через Argon2 выводится общий секрет. Из него получаются
//! ключ XChaCha20-Poly1305 для всех пакетов и «тема» в DHT, по которой участники
//! находят друг друга. Подобрать пароль по теме можно только через Argon2 на каждую попытку.

use anyhow::{Result, anyhow};
use argon2::Argon2;
use chacha20poly1305::aead::{Aead, Generate, Key, KeyInit};
use chacha20poly1305::{XChaCha20Poly1305, XNonce};
use sha2::{Digest, Sha256};

/// Первые байты каждого нашего пакета. Отличает их от ответов STUN.
pub const MAGIC: [u8; 2] = *b"PJ";
const NONCE_LEN: usize = 24;
const TAG_LEN: usize = 16;
/// Сколько байт шифрование добавляет к каждому пакету.
pub const OVERHEAD: usize = MAGIC.len() + NONCE_LEN + TAG_LEN;

/// Общий секрет сети (результат Argon2).
#[derive(Clone)]
pub struct Secret([u8; 32]);

impl Secret {
    pub fn derive(network: &str, password: &str) -> Result<Self> {
        let salt = Sha256::digest(format!("p2pjack/salt/v1/{network}").as_bytes());
        let mut out = [0u8; 32];
        Argon2::default()
            .hash_password_into(password.as_bytes(), &salt[..], &mut out)
            .map_err(|e| anyhow!("argon2: {e}"))?;
        Ok(Self(out))
    }

    fn subkey(&self, label: &str) -> [u8; 32] {
        let mut h = Sha256::new();
        h.update(label.as_bytes());
        h.update(self.0);
        let mut out = [0u8; 32];
        out.copy_from_slice(&h.finalize()[..]);
        out
    }

    /// Infohash для BitTorrent DHT, под которым участники сети объявляют свои адреса.
    pub fn dht_topic(&self) -> [u8; 20] {
        let mut out = [0u8; 20];
        out.copy_from_slice(&self.subkey("p2pjack/dht/v1")[..20]);
        out
    }

    pub fn net_key(&self) -> NetKey {
        let key = Key::<XChaCha20Poly1305>::from(self.subkey("p2pjack/enc/v1"));
        NetKey { cipher: XChaCha20Poly1305::new(&key) }
    }
}

#[derive(Clone)]
pub struct NetKey {
    cipher: XChaCha20Poly1305,
}

impl NetKey {
    /// `MAGIC | nonce | ciphertext+tag`
    pub fn seal(&self, plaintext: &[u8]) -> Vec<u8> {
        let nonce = XNonce::generate();
        let ct = self.cipher.encrypt(&nonce, plaintext).expect("encryption never fails");
        let mut out = Vec::with_capacity(OVERHEAD + plaintext.len());
        out.extend_from_slice(&MAGIC);
        out.extend_from_slice(&nonce[..]);
        out.extend_from_slice(&ct);
        out
    }

    /// `None`, если пакет не наш или зашифрован другим паролем.
    pub fn open(&self, packet: &[u8]) -> Option<Vec<u8>> {
        if packet.len() < OVERHEAD || packet[..MAGIC.len()] != MAGIC {
            return None;
        }
        let (nonce, ct) = packet[MAGIC.len()..].split_at(NONCE_LEN);
        let nonce = XNonce::try_from(nonce).ok()?;
        self.cipher.decrypt(&nonce, ct).ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_and_wrong_password() {
        let a = Secret::derive("room", "secret").unwrap();
        let b = Secret::derive("room", "other").unwrap();
        let sealed = a.net_key().seal(b"hello");
        assert_eq!(a.net_key().open(&sealed).as_deref(), Some(&b"hello"[..]));
        assert!(b.net_key().open(&sealed).is_none());
        assert_ne!(a.dht_topic(), b.dht_topic());
    }

    #[test]
    fn tampered_packet_is_rejected() {
        let key = Secret::derive("room", "secret").unwrap().net_key();
        let mut sealed = key.seal(b"hello");
        *sealed.last_mut().unwrap() ^= 1;
        assert!(key.open(&sealed).is_none());
    }
}
