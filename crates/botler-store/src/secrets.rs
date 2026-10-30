//! Secret values, encrypted at rest with XChaCha20-Poly1305 under a key file kept outside the
//! database. Declarations (names, classes) live in the monorepo; only values live here.

use std::path::Path;

use chacha20poly1305::aead::{Aead, KeyInit};
use chacha20poly1305::{XChaCha20Poly1305, XNonce};
use chrono::Utc;
use rand::RngCore;
use sqlx::Row;

use crate::{Error, Result, Store, ts};

/// The data-encryption key. Never logged, never serialized.
pub struct SecretKey(chacha20poly1305::Key);

impl std::fmt::Debug for SecretKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("SecretKey(..)")
    }
}

impl SecretKey {
    /// Load the key file, creating it (mode 0600) on first run.
    pub fn load_or_create(path: &Path) -> Result<Self> {
        let crypto = |source: std::io::Error| Error::KeyFile { path: path.to_path_buf(), source };
        if path.exists() {
            let bytes = std::fs::read(path).map_err(crypto)?;
            let arr: [u8; 32] = bytes.as_slice().try_into().map_err(|_| Error::KeyLength(path.to_path_buf()))?;
            return Ok(Self(arr.into()));
        }
        let mut key = [0u8; 32];
        rand::rng().fill_bytes(&mut key);
        let mut opts = std::fs::OpenOptions::new();
        opts.write(true).create_new(true);
        #[cfg(unix)]
        std::os::unix::fs::OpenOptionsExt::mode(&mut opts, 0o600);
        let mut f = opts.open(path).map_err(crypto)?;
        std::io::Write::write_all(&mut f, &key).map_err(crypto)?;
        Ok(Self(key.into()))
    }

    /// A key for another purpose (e.g. token signing), derived so that it reveals nothing about
    /// this key and differs per `label`.
    pub fn derive(&self, label: &str) -> [u8; 32] {
        use sha2::{Digest, Sha256};
        let mut h = Sha256::new();
        h.update(b"botler-subkey-v1\0");
        h.update(label.as_bytes());
        h.update(b"\0");
        h.update(self.0.as_slice());
        h.finalize().into()
    }

    pub fn ephemeral() -> Self {
        let mut key = [0u8; 32];
        rand::rng().fill_bytes(&mut key);
        Self(key.into())
    }
}

impl Store {
    pub async fn set_secret(&self, key: &SecretKey, name: &str, value: &str) -> Result<()> {
        let cipher = XChaCha20Poly1305::new(&key.0);
        let mut nonce = [0u8; 24];
        rand::rng().fill_bytes(&mut nonce);
        // The name is bound as associated data so a ciphertext cannot be moved to another name.
        let ct = cipher
            .encrypt(
                &XNonce::from(nonce),
                chacha20poly1305::aead::Payload { msg: value.as_bytes(), aad: name.as_bytes() },
            )
            .map_err(|_| Error::Encrypt(name.into()))?;
        sqlx::query(
            "INSERT INTO secrets (name, nonce, ciphertext, updated_at) VALUES (?, ?, ?, ?)
             ON CONFLICT (name) DO UPDATE SET nonce = excluded.nonce, ciphertext = excluded.ciphertext, updated_at = excluded.updated_at",
        )
        .bind(name)
        .bind(nonce.as_slice())
        .bind(ct)
        .bind(ts(Utc::now()))
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn secret(&self, key: &SecretKey, name: &str) -> Result<Option<String>> {
        let Some(r) = sqlx::query("SELECT nonce, ciphertext FROM secrets WHERE name = ?")
            .bind(name)
            .fetch_optional(&self.pool)
            .await?
        else {
            return Ok(None);
        };
        let nonce: Vec<u8> = r.get("nonce");
        let ct: Vec<u8> = r.get("ciphertext");
        let nonce: [u8; 24] = nonce
            .try_into()
            .map_err(|_| Error::Corrupt { table: "secrets", detail: format!("bad nonce for {name}") })?;
        let cipher = XChaCha20Poly1305::new(&key.0);
        let pt = cipher
            .decrypt(&XNonce::from(nonce), chacha20poly1305::aead::Payload { msg: &ct, aad: name.as_bytes() })
            .map_err(|_| Error::Decrypt(name.into()))?;
        String::from_utf8(pt).map(Some).map_err(|_| Error::NotUtf8(name.into()))
    }

    /// Names that have a stored value (never the values).
    pub async fn secret_names(&self) -> Result<Vec<String>> {
        let rows = sqlx::query("SELECT name FROM secrets ORDER BY name").fetch_all(&self.pool).await?;
        Ok(rows.iter().map(|r| r.get("name")).collect())
    }

    pub async fn delete_secret(&self, name: &str) -> Result<bool> {
        let r = sqlx::query("DELETE FROM secrets WHERE name = ?").bind(name).execute(&self.pool).await?;
        Ok(r.rows_affected() == 1)
    }
}
