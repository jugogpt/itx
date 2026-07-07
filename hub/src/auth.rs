use tracing::*;

use btclib::crypto::{PublicKey, Signature};
use btclib::sha256::Hash;
use chrono::{DateTime, Duration, Utc};
use dashmap::DashMap;
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use static_init::dynamic;

/// How far a request's claimed timestamp may drift from our own clock
/// before we reject it outright. Bounds how long a captured request stays
/// replayable, and doubles as the replay-guard's retention window.
const MAX_REQUEST_DRIFT_SECONDS: i64 = 120;

#[dynamic]
static SEEN_SIGNATURES: DashMap<Vec<u8>, DateTime<Utc>> = DashMap::new();

#[derive(Debug, thiserror::Error)]
pub enum AuthError {
    #[error("request timestamp is too far from the server's clock")]
    ClockDrift,
    #[error("request has already been used (possible replay)")]
    Replayed,
    #[error("signature does not match the claimed public key")]
    BadSignature,
    #[error("malformed public key: {0}")]
    BadPublicKey(String),
    #[error("malformed signature: {0}")]
    BadSignatureEncoding(String),
}

/// A request signed by the agent making it.
///
/// `pubkey`/`signature` travel as hex strings rather than btclib's
/// internal CBOR shape, and what gets signed is a plain canonical string
/// -- not a Rust/CBOR-specific encoding -- so that any HTTP client in any
/// language (not just Rust) can construct a valid request. The exact
/// recipe (see `signing_string`) is: `"{pubkey_hex}:{timestamp_rfc3339}:{payload_as_json}"`,
/// SHA256'd and then secp256k1/ECDSA-signed.
#[derive(Debug, Deserialize, Serialize)]
pub struct SignedEnvelope<T> {
    pub pubkey: String,
    pub timestamp: DateTime<Utc>,
    pub payload: T,
    pub signature: String,
}

impl<T: Serialize + DeserializeOwned> SignedEnvelope<T> {
    fn signing_string(&self) -> Result<String, AuthError> {
        let payload_json = serde_json::to_string(&self.payload)
            .map_err(|e| AuthError::BadPublicKey(format!("payload not serializable: {e}")))?;
        Ok(format!(
            "{}:{}:{}",
            self.pubkey,
            self.timestamp.to_rfc3339(),
            payload_json
        ))
    }

    /// Verifies the envelope's timestamp, replay status, and signature,
    /// returning the verified public key on success. Every state-changing
    /// endpoint must call this before acting on a request; read-only
    /// endpoints need no envelope at all.
    pub fn verify(&self) -> Result<PublicKey, AuthError> {
        let now = Utc::now();
        if (now - self.timestamp).abs() > Duration::seconds(MAX_REQUEST_DRIFT_SECONDS) {
            return Err(AuthError::ClockDrift);
        }

        let pubkey_bytes =
            hex::decode(&self.pubkey).map_err(|e| AuthError::BadPublicKey(e.to_string()))?;
        let pubkey = PublicKey::from_sec1_bytes(&pubkey_bytes)
            .map_err(|e| AuthError::BadPublicKey(e.to_string()))?;

        let signature_bytes = hex::decode(&self.signature)
            .map_err(|e| AuthError::BadSignatureEncoding(e.to_string()))?;
        let signature = Signature::from_bytes(&signature_bytes)
            .map_err(|e| AuthError::BadSignatureEncoding(e.to_string()))?;

        let hash = Hash::hash_bytes(self.signing_string()?.as_bytes());
        if !signature.verify(&hash, &pubkey) {
            return Err(AuthError::BadSignature);
        }

        // Only mark as seen once the signature is confirmed genuine --
        // otherwise anyone could burn arbitrary signature slots with junk
        // bytes. A real signature is unforgeable, so this can only ever
        // be inserted by whoever actually holds the private key.
        if SEEN_SIGNATURES
            .insert(signature_bytes, now)
            .is_some()
        {
            return Err(AuthError::Replayed);
        }

        Ok(pubkey)
    }
}

/// Evicts replay-guard entries old enough that their originating request
/// would already fail the clock-drift check on its own -- keeps this from
/// growing forever on a long-running node. Call periodically from a
/// background sweep.
pub fn cleanup_replay_guard() {
    let cutoff = Utc::now() - Duration::seconds(MAX_REQUEST_DRIFT_SECONDS);
    SEEN_SIGNATURES.retain(|_, seen_at| *seen_at > cutoff);
}
