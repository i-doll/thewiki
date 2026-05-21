//! Password hashing & verification, parameterised by [`Argon2Config`].
//!
//! The trait + concrete impl split looks like overkill for one algorithm, but
//! the seam matters for tests (an integration test can plug in a "dummy"
//! hasher whose `hash`/`verify` are constant-time but instant) and for the
//! eventual cost-tuning pass once we have profiling data.
//!
//! Argon2id is the OWASP-recommended choice. We always go through the PHC
//! string format (`$argon2id$v=19$m=…,t=…,p=…$salt$hash`) so verification
//! re-derives parameters from the stored hash and an operator who bumps
//! `auth.argon2.memory_kib` in `thewiki.toml` doesn't invalidate existing
//! passwords — they're just re-hashed lazily on next login (#35 will wire that
//! re-hash flow).
//!
//! Constant-time guarantee: `Argon2::verify_password` (from the `argon2`
//! crate) uses `subtle::ConstantTimeEq` internally on the final digest
//! comparison, so a successful run takes the same wall-clock time regardless
//! of which byte differs.

use argon2::password_hash::{
    PasswordHash, PasswordHasher as _, PasswordVerifier, SaltString, rand_core::OsRng,
};
use argon2::{Algorithm, Argon2, Params, Version};

use crate::auth::error::AuthError;
use crate::config::Argon2Config;

/// The hash-or-verify surface the auth handlers depend on.
///
/// `Send + Sync` so the impl can live behind an `Arc` in app state.
pub trait PasswordHasher: Send + Sync {
    /// Hash `password` into a PHC string suitable for the `users.password_hash`
    /// column.
    ///
    /// # Errors
    ///
    /// [`AuthError::HashFailure`] if the underlying primitive errors (e.g.
    /// `Params::new` rejects the configured cost).
    fn hash(&self, password: &str) -> Result<String, AuthError>;

    /// Verify `password` against a stored PHC string in constant time.
    ///
    /// Returns `Ok(true)` on match, `Ok(false)` on mismatch, and an error
    /// **only** when the hash string itself is unparseable (i.e. the row is
    /// corrupt). A wrong password is `Ok(false)`, *not* an error.
    ///
    /// # Errors
    ///
    /// [`AuthError::HashFailure`] when the PHC string is malformed.
    fn verify(&self, password: &str, hash: &str) -> Result<bool, AuthError>;
}

/// Argon2id implementation. Parameters come from [`Argon2Config`].
#[derive(Debug, Clone)]
pub struct Argon2Hasher {
    params: Params,
}

impl Argon2Hasher {
    /// Build an Argon2id hasher from the auth config.
    ///
    /// # Errors
    ///
    /// [`AuthError::HashFailure`] if the parameters are out of the underlying
    /// crate's accepted range (this should be caught at [`Config::validate`]
    /// time, but the boundary is enforced here too as a defence in depth).
    ///
    /// [`Config::validate`]: crate::config::Config::validate
    pub fn new(cfg: Argon2Config) -> Result<Self, AuthError> {
        let params = Params::new(
            cfg.memory_kib,
            cfg.iterations,
            cfg.parallelism,
            // Default output length (32 bytes) matches `argon2`'s default and
            // the OWASP recommendation; we don't expose this as a knob.
            None,
        )
        .map_err(|e| AuthError::HashFailure(format!("argon2 params: {e}")))?;
        Ok(Self { params })
    }

    /// Borrow the configured Argon2 primitive (used by [`verify`]).
    fn argon2(&self) -> Argon2<'_> {
        Argon2::new(Algorithm::Argon2id, Version::V0x13, self.params.clone())
    }

    /// A pre-baked PHC string fed to `verify` on the "user not found" branch
    /// so we still do the full Argon2 work and don't leak via response time.
    ///
    /// This is a hash of an empty string with the configured params; the
    /// caller compares the *submitted* password against it and necessarily
    /// gets `false`. The point is the wall-clock time, not the result.
    ///
    /// # Errors
    ///
    /// As [`Self::hash`].
    pub fn dummy_hash_for_timing(&self) -> Result<String, AuthError> {
        self.hash("")
    }
}

impl PasswordHasher for Argon2Hasher {
    fn hash(&self, password: &str) -> Result<String, AuthError> {
        let salt = SaltString::generate(&mut OsRng);
        let hash = self
            .argon2()
            .hash_password(password.as_bytes(), &salt)
            .map_err(|e| AuthError::HashFailure(format!("argon2 hash: {e}")))?;
        Ok(hash.to_string())
    }

    fn verify(&self, password: &str, hash: &str) -> Result<bool, AuthError> {
        let parsed = PasswordHash::new(hash)
            .map_err(|e| AuthError::HashFailure(format!("argon2 parse: {e}")))?;
        // `verify_password` returns Err on mismatch *as well as* on parse
        // failure — but at this point we've already parsed, so any error is
        // a mismatch. The match below preserves that distinction explicitly.
        match self.argon2().verify_password(password.as_bytes(), &parsed) {
            Ok(()) => Ok(true),
            Err(argon2::password_hash::Error::Password) => Ok(false),
            Err(e) => Err(AuthError::HashFailure(format!("argon2 verify: {e}"))),
        }
    }
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used)]
mod tests {
    use super::*;

    fn test_cfg() -> Argon2Config {
        // Use parameters at the OWASP floor so the test stays under a few
        // hundred milliseconds. Production tuning is `>= 64 MiB / 3 iters`,
        // enforced by `Config::validate`.
        Argon2Config {
            memory_kib: 19_456,
            iterations: 2,
            parallelism: 1,
        }
    }

    #[test]
    fn hash_then_verify_roundtrip() {
        let h = Argon2Hasher::new(test_cfg()).expect("hasher");
        let phc = h.hash("correct horse battery staple").expect("hash");
        assert!(phc.starts_with("$argon2id$"));
        assert!(
            h.verify("correct horse battery staple", &phc)
                .expect("verify")
        );
    }

    #[test]
    fn verify_returns_false_on_mismatch() {
        let h = Argon2Hasher::new(test_cfg()).expect("hasher");
        let phc = h.hash("password").expect("hash");
        assert!(!h.verify("not the password", &phc).expect("verify"));
    }

    #[test]
    fn verify_errors_on_malformed_phc() {
        let h = Argon2Hasher::new(test_cfg()).expect("hasher");
        let err = h
            .verify("anything", "$not$an$argon2$hash")
            .expect_err("malformed");
        assert!(matches!(err, AuthError::HashFailure(_)));
    }

    #[test]
    fn dummy_hash_for_timing_is_a_valid_phc_string() {
        let h = Argon2Hasher::new(test_cfg()).expect("hasher");
        let dummy = h.dummy_hash_for_timing().expect("dummy");
        // The whole point is that `verify` against it returns false in
        // constant time. We don't care what's in it.
        assert!(!h.verify("anything", &dummy).expect("verify"));
    }
}
