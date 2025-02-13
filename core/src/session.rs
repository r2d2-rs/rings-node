#![warn(missing_docs)]

//! Signing/verifying and encrypting/decrypting messages.
//!
//! This module provides a mechanism for node A to verify that the message received was sent by node B.
//! It also allows node A to obtain the public key for sending encrypted messages to node B.
//!
//! Considering security factors, asking user to provide private key is not practical.
//! On the contrary, we generate a delegated private key and let user sign it.
//!
//! See [SessionManager] and [SessionManagerBuilder] for details.

use std::str::FromStr;

use rings_derive::wasm_export;
use serde::Deserialize;
use serde::Serialize;

use crate::consts::DEFAULT_SESSION_TTL_MS;
use crate::dht::Did;
use crate::ecc::signers;
use crate::ecc::PublicKey;
use crate::ecc::SecretKey;
use crate::error::Error;
use crate::error::Result;
use crate::utils;

fn pack_session(session_id: Did, ts_ms: u128, ttl_ms: usize) -> String {
    format!("{}\n{}\n{}", session_id, ts_ms, ttl_ms)
}

/// SessionManagerBuilder is used to build a [SessionManager].
///
/// Firstly, you need to provide the authorizer's entity and type to `new` method.
/// Then you can call `pack_session` to get the session dump for signing.
/// After signing, you can call `sig` to set the signature back to builder.
/// Finally, you can call `build` to get the [SessionManager].
#[wasm_export]
pub struct SessionManagerBuilder {
    session_key: SecretKey,
    /// Authorizer of session.
    authorizer_entity: String,
    /// Authorizer of session.
    authorizer_type: String,
    /// Session's lifetime
    ttl_ms: usize,
    /// Timestamp when session created
    ts_ms: u128,
    /// Signature
    sig: Vec<u8>,
}

/// SessionManager holds the [Session] and its delegated private key.
/// To prove that the message was sent by the [Authorizer] of [Session],
/// we need to attach session and the signature signed by session_key to the payload.
///
/// SessionManager provide a `session` method to clone the session.
/// SessionManager also provide `sign` method to sign a message.
///
/// To verify the session, use `verify_self()` method of [Session].
/// To verify a message, use `verify(msg, sig)` method of [Session].
#[wasm_export]
#[derive(Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct SessionManager {
    /// Session
    session: Session,
    /// The private key of session. Used for signing and decrypting.
    session_key: SecretKey,
}

/// Session is used to verify the message.
/// It's serializable and can be attached to the message payload.
///
/// To verify the session is provided by the authorizer, use session.verify_self().
/// To verify the message, use session.verify(msg, sig).
#[derive(Deserialize, Serialize, PartialEq, Eq, Debug, Clone)]
pub struct Session {
    /// Did of session
    session_id: Did,
    /// Authorizer of session
    authorizer: Authorizer,
    /// Session's lifetime
    ttl_ms: usize,
    /// Timestamp when session created
    ts_ms: u128,
    /// Signature to verify that the session was signed by the authorizer.
    sig: Vec<u8>,
}

/// We will support as many protocols/algorithms as possible.
/// Currently, it comprises Secp256k1, EIP191, BIP137, and Ed25519.
/// We welcome any issues and PRs for additional implementations.
#[derive(Deserialize, Serialize, PartialEq, Eq, Debug, Clone)]
pub enum Authorizer {
    /// ecdsa
    Secp256k1(Did),
    /// ref: <https://eips.ethereum.org/EIPS/eip-191>
    EIP191(Did),
    /// bitcoin bip137 ref: <https://github.com/bitcoin/bips/blob/master/bip-0137.mediawiki>
    BIP137(Did),
    /// ed25519
    Ed25519(PublicKey),
}

impl TryFrom<(String, String)> for Authorizer {
    type Error = Error;

    fn try_from((authorizer_entity, authorizer_type): (String, String)) -> Result<Self> {
        match authorizer_type.as_str() {
            "secp256k1" => Ok(Authorizer::Secp256k1(Did::from_str(&authorizer_entity)?)),
            "eip191" => Ok(Authorizer::EIP191(Did::from_str(&authorizer_entity)?)),
            "bip137" => Ok(Authorizer::BIP137(Did::from_str(&authorizer_entity)?)),
            "ed25519" => Ok(Authorizer::Ed25519(PublicKey::try_from_b58t(
                &authorizer_entity,
            )?)),
            _ => Err(Error::UnknownAuthorizer),
        }
    }
}

// A SessionManager can be converted to a string using JSON and then encoded with base58.
// To load the SessionManager from a string, use `SessionManager::from_str`.
impl FromStr for SessionManager {
    type Err = Error;

    fn from_str(s: &str) -> Result<Self> {
        let s = base58_monero::decode_check(s).map_err(|_| Error::Decode)?;
        let session_manager: SessionManager =
            serde_json::from_slice(&s).map_err(Error::Deserialize)?;
        Ok(session_manager)
    }
}

#[wasm_export]
impl SessionManagerBuilder {
    /// Create a new SessionManagerBuilder.
    /// The "authorizer_type" is lower case of [Authorizer] variant.
    /// The "authorizer_entity" refers to the entity that is encapsulated by the [Authorizer] variant, in string format.
    pub fn new(authorizer_entity: String, authorizer_type: String) -> SessionManagerBuilder {
        let session_key = SecretKey::random();
        Self {
            session_key,
            authorizer_entity,
            authorizer_type,
            ttl_ms: DEFAULT_SESSION_TTL_MS,
            ts_ms: utils::get_epoch_ms(),
            sig: vec![],
        }
    }

    /// This is a helper method to let user know if the authorizer params is valid.
    pub fn validate_authorizer(&self) -> bool {
        Authorizer::try_from((self.authorizer_entity.clone(), self.authorizer_type.clone()))
            .map_err(|e| {
                tracing::warn!("validate_authorizer error: {:?}", e);
                e
            })
            .is_ok()
    }

    /// Packs the session into a string for signing.
    pub fn pack_session(&self) -> String {
        pack_session(self.session_key.address().into(), self.ts_ms, self.ttl_ms)
    }

    /// Set the signature of session that signed by authorizer.
    pub fn sig(mut self, sig: Vec<u8>) -> Self {
        self.sig = sig;
        self
    }

    /// Set the lifetime of session.
    pub fn ttl(mut self, ttl_ms: usize) -> Self {
        self.ttl_ms = ttl_ms;
        self
    }

    /// Build the [SessionManager].
    pub fn build(self) -> Result<SessionManager> {
        let authorizer = Authorizer::try_from((self.authorizer_entity, self.authorizer_type))?;
        let session = Session {
            session_id: self.session_key.address().into(),
            authorizer,
            ttl_ms: self.ttl_ms,
            ts_ms: self.ts_ms,
            sig: self.sig,
        };

        session.verify_self()?;

        Ok(SessionManager {
            session,
            session_key: self.session_key,
        })
    }
}

impl Session {
    /// Pack the session into a string for verification or public key recovery.
    pub fn pack(&self) -> String {
        pack_session(self.session_id, self.ts_ms, self.ttl_ms)
    }

    /// Check session is expired or not.
    pub fn is_expired(&self) -> bool {
        let now = utils::get_epoch_ms();
        now > self.ts_ms + self.ttl_ms as u128
    }

    /// Verify session.
    pub fn verify_self(&self) -> Result<()> {
        if self.is_expired() {
            return Err(Error::SessionExpired);
        }

        let auth_str = self.pack();

        if !(match self.authorizer {
            Authorizer::Secp256k1(did) => {
                signers::secp256k1::verify(&auth_str, &did.into(), &self.sig)
            }
            Authorizer::EIP191(did) => signers::eip191::verify(&auth_str, &did.into(), &self.sig),
            Authorizer::BIP137(did) => signers::bip137::verify(&auth_str, &did.into(), &self.sig),
            Authorizer::Ed25519(pk) => {
                signers::ed25519::verify(&auth_str, &pk.address(), &self.sig, pk)
            }
        }) {
            return Err(Error::VerifySignatureFailed);
        }

        Ok(())
    }

    /// Verify message.
    pub fn verify(&self, msg: &str, sig: impl AsRef<[u8]>) -> Result<()> {
        self.verify_self()?;
        if !signers::secp256k1::verify(msg, &self.session_id, sig) {
            return Err(Error::VerifySignatureFailed);
        }
        Ok(())
    }

    /// Get public key from session for encryption.
    pub fn authorizer_pubkey(&self) -> Result<PublicKey> {
        let auth_str = self.pack();
        match self.authorizer {
            Authorizer::Secp256k1(_) => signers::secp256k1::recover(&auth_str, &self.sig),
            Authorizer::BIP137(_) => signers::bip137::recover(&auth_str, &self.sig),
            Authorizer::EIP191(_) => signers::eip191::recover(&auth_str, &self.sig),
            Authorizer::Ed25519(pk) => Ok(pk),
        }
    }

    /// Get authorizer did.
    pub fn authorizer_did(&self) -> Did {
        match self.authorizer {
            Authorizer::Secp256k1(did) => did,
            Authorizer::BIP137(did) => did,
            Authorizer::EIP191(did) => did,
            Authorizer::Ed25519(pk) => pk.address().into(),
        }
    }
}

impl SessionManager {
    /// Generate Session with private key.
    /// Only use it for unittest.
    pub fn new_with_seckey(key: &SecretKey) -> Result<Self> {
        let authorizer_entity = Did::from(key.address()).to_string();
        let authorizer_type = "secp256k1".to_string();

        let mut builder = SessionManagerBuilder::new(authorizer_entity, authorizer_type);

        let sig = key.sign(&builder.pack_session());
        builder = builder.sig(sig.to_vec());

        builder.build()
    }

    /// Get session from SessionManager.
    pub fn session(&self) -> Session {
        self.session.clone()
    }

    /// Sign message with session.
    pub fn sign(&self, msg: &str) -> Result<Vec<u8>> {
        let key = self.session_key;
        Ok(signers::secp256k1::sign_raw(key, msg).to_vec())
    }

    /// Get authorizer did from session.
    pub fn authorizer_did(&self) -> Did {
        self.session.authorizer_did()
    }

    /// Dump session_manager to string, allowing user to save it in a config file.
    /// It can be restored using `SessionManager::from_str`.
    pub fn dump(&self) -> Result<String> {
        let s = serde_json::to_string(&self).map_err(|_| Error::SerializeError)?;
        base58_monero::encode_check(s.as_bytes()).map_err(|_| Error::Encode)
    }
}

#[cfg(test)]
mod test {
    use super::*;

    #[test]
    pub fn test_session_verify() {
        let key = SecretKey::random();
        let sm = SessionManager::new_with_seckey(&key).unwrap();
        let session = sm.session();
        assert!(session.verify_self().is_ok());
    }

    #[test]
    pub fn test_authorizer_pubkey() {
        let key = SecretKey::random();
        let sm = SessionManager::new_with_seckey(&key).unwrap();
        let session = sm.session();
        let pubkey = session.authorizer_pubkey().unwrap();
        assert_eq!(key.pubkey(), pubkey);
    }

    #[test]
    pub fn test_dump_restore() {
        let key = SecretKey::random();
        let sm = SessionManager::new_with_seckey(&key).unwrap();
        let dump = sm.dump().unwrap();
        let sm2 = SessionManager::from_str(&dump).unwrap();
        assert_eq!(sm, sm2);
    }
}
