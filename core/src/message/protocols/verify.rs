//! Implementation of Message Verification.
#![warn(missing_docs)]

use std::fmt::Write;

use serde::Deserialize;
use serde::Serialize;

use crate::ecc::signers;
use crate::ecc::PublicKey;
use crate::error::Error;
use crate::error::Result;
use crate::session::Session;

/// Message Verification is based on session, and sig.
/// it also included ttl time and created ts.
#[derive(Deserialize, Serialize, Debug, Clone, PartialEq, Eq)]
pub struct MessageVerification {
    pub session: Session,
    pub ttl_ms: usize,
    pub ts_ms: u128,
    pub sig: Vec<u8>,
}

impl MessageVerification {
    /// Verify a MessageVerification
    pub fn verify<T>(&self, data: &T) -> bool
    where T: Serialize {
        let Ok(msg) = self.msg(data) else {
            tracing::warn!("MessageVerification pack_msg failed");
            return false;
        };

        self.session
            .verify(&msg, &self.sig)
            .map_err(|e| {
                tracing::warn!("MessageVerification verify failed: {:?}", e);
            })
            .is_ok()
    }

    /// Recover publickey from packed message.
    pub fn session_pubkey<T>(&self, data: &T) -> Result<PublicKey>
    where T: Serialize {
        let msg = self.msg(data)?;
        signers::secp256k1::recover(&msg, &self.sig)
    }

    /// Pack Message to string, and attach ts and ttl on it.
    pub fn pack_msg<T>(data: &T, ts_ms: u128, ttl_ms: usize) -> Result<String>
    where T: Serialize {
        let mut msg = serde_json::to_string(data).map_err(|_| Error::SerializeToString)?;
        write!(msg, "\n{}\n{}", ts_ms, ttl_ms).map_err(|_| Error::SerializeToString)?;
        Ok(msg)
    }

    /// Alias of pack_msg.
    fn msg<T>(&self, data: &T) -> Result<String>
    where T: Serialize {
        Self::pack_msg(data, self.ts_ms, self.ttl_ms)
    }
}
