#![allow(dead_code)]
//! Shared helpers for integration tests against a live JMAP server (Fastmail).
//!
//! Requires environment variables:
//!   NEVERLIGHT_MAIL_JMAP_TOKEN — Fastmail API token (fmu1-...) or app password (mu1-...)
//!   NEVERLIGHT_MAIL_USER       — Account email address
//!
//! Source from `.envrc` in the crate root.

use neverlight_mail_core::session::JmapSession;
use neverlight_mail_core::store::CacheHandle;

pub const FASTMAIL_SESSION_URL: &str = "https://api.fastmail.com/jmap/session";

macro_rules! skip_if_no_env {
    () => {
        if std::env::var("NEVERLIGHT_MAIL_JMAP_TOKEN").is_err()
            || std::env::var("NEVERLIGHT_MAIL_USER").is_err()
        {
            eprintln!("SKIP: NEVERLIGHT_MAIL_JMAP_TOKEN or NEVERLIGHT_MAIL_USER not set");
            return;
        }
    };
}
pub(crate) use skip_if_no_env;

/// Connect using the appropriate auth method based on token prefix.
pub async fn connect_client() -> (JmapSession, neverlight_mail_core::client::JmapClient) {
    let token = std::env::var("NEVERLIGHT_MAIL_JMAP_TOKEN").expect("NEVERLIGHT_MAIL_JMAP_TOKEN");
    let user = std::env::var("NEVERLIGHT_MAIL_USER").expect("NEVERLIGHT_MAIL_USER");

    if token.starts_with("fmu1-") {
        JmapSession::connect_with_token(FASTMAIL_SESSION_URL, &token)
            .await
            .expect("Bearer auth failed")
    } else {
        JmapSession::connect_with_basic(FASTMAIL_SESSION_URL, &user, &token)
            .await
            .expect("Basic auth failed")
    }
}

/// Open a throwaway cache for integration tests.
pub fn test_cache() -> CacheHandle {
    CacheHandle::open("integration-test").expect("open cache")
}
