//! [`Session`] — an authenticated login bound to a [`User`](crate::user::User).
//!
//! A session is the server-side record of a successful login. Its [`SessionId`]
//! is opaque to clients (handed back as a cookie value) but to anyone holding
//! it, knowledge of the ID is enough to act as the user — so storage layers
//! treat the ID as security-sensitive material.
//!
//! Sessions carry an `expires_at`; once that point in time has passed, the
//! storage layer reports the session as `NotFound`. `last_seen_at` is updated
//! on each authenticated request so the admin UI can show staleness; it does
//! **not** affect expiry.
//!
//! The optional `user_agent` and `ip_address` are captured for the admin UI
//! and audit story (see #13) and are not consulted for auth decisions.

use serde::{Deserialize, Serialize};
use time::OffsetDateTime;
use utoipa::ToSchema;

use crate::id::{SessionId, UserId};

/// A server-side authentication session.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct Session {
    /// Opaque session identifier; doubles as the bearer cookie value.
    pub id: SessionId,
    /// The authenticated user.
    pub user_id: UserId,
    /// When the session was first issued.
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
    /// When the session expires. Lookups past this point return `NotFound`.
    #[serde(with = "time::serde::rfc3339")]
    pub expires_at: OffsetDateTime,
    /// When the session was last seen (bumped per authenticated request).
    #[serde(with = "time::serde::rfc3339")]
    pub last_seen_at: OffsetDateTime,
    /// User-Agent header captured at issuance, if any. Free-form.
    pub user_agent: Option<String>,
    /// IP address captured at issuance, if any. Free-form string so we can
    /// keep IPv4/IPv6 in one column without driver-specific types.
    pub ip_address: Option<String>,
}

impl Session {
    /// Convenience: is this session past its expiry as of `now`?
    #[must_use]
    pub fn is_expired_at(&self, now: OffsetDateTime) -> bool {
        now >= self.expires_at
    }
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used, reason = "ergonomic tests")]
mod tests {
    use super::*;

    #[test]
    fn round_trips_serde() {
        let now = OffsetDateTime::from_unix_timestamp(1_700_000_000).expect("ts");
        let session = Session {
            id: SessionId::new(),
            user_id: UserId::new(),
            created_at: now,
            expires_at: now + time::Duration::hours(24),
            last_seen_at: now,
            user_agent: Some("curl/8.0".into()),
            ip_address: Some("127.0.0.1".into()),
        };
        let json = serde_json::to_string(&session).expect("serialise");
        let parsed: Session = serde_json::from_str(&json).expect("deserialise");
        assert_eq!(parsed, session);
    }

    #[test]
    fn is_expired_compares_against_supplied_now() {
        let issued = OffsetDateTime::from_unix_timestamp(1_700_000_000).expect("ts");
        let session = Session {
            id: SessionId::new(),
            user_id: UserId::new(),
            created_at: issued,
            expires_at: issued + time::Duration::hours(1),
            last_seen_at: issued,
            user_agent: None,
            ip_address: None,
        };
        assert!(!session.is_expired_at(issued));
        assert!(session.is_expired_at(issued + time::Duration::hours(2)));
    }
}
