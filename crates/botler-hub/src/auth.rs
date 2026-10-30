//! Bearer tokens for containers and the user.
//!
//! Tokens are stateless: `<role>.<subject>.<expiry>.<mac>`, HMAC-SHA256 over the first three
//! parts with a key derived from the hub's secret key file. Nothing to store, nothing to clean
//! up, and a hub restart with the same key file keeps tokens valid until they expire.

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use chrono::{DateTime, Duration, Utc};
use hmac::{Hmac, Mac};
use sha2::Sha256;

type HmacSha256 = Hmac<Sha256>;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Principal {
    /// The single human user (web app, CLI).
    User,
    /// An agent instance.
    Agent(String),
    /// One container run of a job or service.
    Run { run_id: String },
}

impl Principal {
    fn encode(&self) -> (String, String) {
        match self {
            Self::User => ("user".into(), "-".into()),
            Self::Agent(n) => ("agent".into(), n.clone()),
            Self::Run { run_id } => ("run".into(), run_id.clone()),
        }
    }

    fn decode(role: &str, subject: &str) -> Option<Self> {
        match role {
            "user" => Some(Self::User),
            "agent" => Some(Self::Agent(subject.to_string())),
            "run" => Some(Self::Run { run_id: subject.to_string() }),
            _ => None,
        }
    }
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum AuthError {
    #[error("malformed token")]
    Malformed,
    #[error("bad token signature")]
    BadSignature,
    #[error("token expired")]
    Expired,
}

#[derive(Clone)]
pub struct Tokens {
    key: [u8; 32],
}

impl std::fmt::Debug for Tokens {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Tokens(..)")
    }
}

impl Tokens {
    pub fn new(key: [u8; 32]) -> Self {
        Self { key }
    }

    pub fn from_secret_key(k: &botler_store::SecretKey) -> Self {
        Self::new(k.derive("tokens"))
    }

    fn mac(&self, msg: &str) -> HmacSha256 {
        // HMAC accepts keys of any length; 32 bytes is always valid.
        let mut m = <HmacSha256 as Mac>::new_from_slice(&self.key).expect("HMAC accepts any key length");
        m.update(msg.as_bytes());
        m
    }

    pub fn issue(&self, who: &Principal, ttl: Duration, now: DateTime<Utc>) -> String {
        let (role, subject) = who.encode();
        let subject = URL_SAFE_NO_PAD.encode(subject);
        let body = format!("{role}.{subject}.{}", (now + ttl).timestamp());
        let sig = URL_SAFE_NO_PAD.encode(self.mac(&body).finalize().into_bytes());
        format!("{body}.{sig}")
    }

    pub fn verify(&self, token: &str, now: DateTime<Utc>) -> Result<Principal, AuthError> {
        let mut parts = token.rsplitn(2, '.');
        let (Some(sig), Some(body)) = (parts.next(), parts.next()) else { return Err(AuthError::Malformed) };
        let sig = URL_SAFE_NO_PAD.decode(sig).map_err(|_| AuthError::Malformed)?;
        self.mac(body).verify_slice(&sig).map_err(|_| AuthError::BadSignature)?;
        let [role, subject, expiry] = body.split('.').collect::<Vec<_>>()[..] else { return Err(AuthError::Malformed) };
        let expiry: i64 = expiry.parse().map_err(|_| AuthError::Malformed)?;
        if now.timestamp() > expiry {
            return Err(AuthError::Expired);
        }
        let subject = URL_SAFE_NO_PAD.decode(subject).map_err(|_| AuthError::Malformed)?;
        let subject = String::from_utf8(subject).map_err(|_| AuthError::Malformed)?;
        Principal::decode(role, &subject).ok_or(AuthError::Malformed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_tamper_expiry() {
        let t = Tokens::new([1; 32]);
        let now = Utc::now();
        let tok = t.issue(&Principal::Agent("main.x".into()), Duration::hours(1), now);
        assert_eq!(t.verify(&tok, now), Ok(Principal::Agent("main.x".into())));
        assert_eq!(t.verify(&tok, now + Duration::hours(2)), Err(AuthError::Expired));
        let forged = tok.replacen("agent", "user", 1);
        assert_eq!(t.verify(&forged, now), Err(AuthError::BadSignature));
        assert_eq!(Tokens::new([2; 32]).verify(&tok, now), Err(AuthError::BadSignature));
        assert_eq!(t.verify("garbage", now), Err(AuthError::Malformed));
    }
}
