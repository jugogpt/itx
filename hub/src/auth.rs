use tracing::*;

use btclib::crypto::PublicKey;
pub use btclib::envelope::{EnvelopeError as AuthError, SignedEnvelope};
use btclib::envelope::MAX_REQUEST_DRIFT_SECONDS;
use chrono::{DateTime, Duration, Utc};
use dashmap::DashMap;
use serde::{de::DeserializeOwned, Serialize};
use static_init::dynamic;

#[dynamic]
static SEEN_SIGNATURES: DashMap<Vec<u8>, DateTime<Utc>> = DashMap::new();

/// Hub-side verification: the shared signature/timestamp recipe (see
/// `btclib::envelope::SignedEnvelope::verify_signature`) plus a replay
/// check against this process's in-memory cache -- the one part of
/// verification that's inherently server-side state, not something a
/// client SDK needs or could share. Kept as an extension trait (rather
/// than moving the whole thing into btclib) so every existing
/// `envelope.verify()` call site in `handlers.rs` needed no changes
/// beyond importing this trait.
pub trait VerifyEnvelope {
    fn verify(&self) -> Result<PublicKey, AuthError>;
}

impl<T: Serialize + DeserializeOwned> VerifyEnvelope for SignedEnvelope<T> {
    fn verify(&self) -> Result<PublicKey, AuthError> {
        let now = Utc::now();
        let pubkey = self.verify_signature(now)?;

        // Only mark as seen once the signature is confirmed genuine --
        // otherwise anyone could burn arbitrary signature slots with junk
        // bytes. A real signature is unforgeable, so this can only ever
        // be inserted by whoever actually holds the private key. The hex
        // decode below is already known to succeed -- verify_signature
        // just decoded this exact same field.
        let signature_bytes = hex::decode(&self.signature)
            .expect("verify_signature already validated this hex string");
        if SEEN_SIGNATURES.insert(signature_bytes, now).is_some() {
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
