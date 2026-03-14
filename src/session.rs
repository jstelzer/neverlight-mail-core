//! JMAP Session management (RFC 8620 §2).
//!
//! Handles `.well-known/jmap` discovery, capability negotiation,
//! and construction of `JmapClient` from session data.

use crate::client::JmapClient;
use crate::config::AccountConfig;

/// Parsed JMAP Session object (RFC 8620 §2).
#[derive(Debug, Clone)]
pub struct JmapSession {
    pub api_url: String,
    pub upload_url: String,
    pub download_url: String,
    pub event_source_url: Option<String>,
    pub account_id: String,
    pub state: String,
    pub max_objects_in_get: u32,
    pub max_objects_in_set: u32,
    pub max_calls_in_request: u32,
    /// Server advertises push (WebSocket or EventSource).
    pub supports_push: bool,
    /// Server advertises `urn:ietf:params:jmap:submission`.
    pub supports_submission: bool,
}

impl JmapSession {
    /// Connect to a JMAP server using an AccountConfig.
    ///
    /// For app passwords, auto-detects Bearer vs Basic from token prefix.
    /// For OAuth, uses the access token as Bearer (caller must refresh first).
    ///
    /// Auto-heals stale capability flags in the on-disk config when the
    /// server's advertised capabilities differ from what was saved.
    pub async fn connect(config: &AccountConfig) -> Result<(Self, JmapClient), String> {
        use crate::config::AuthMethod;
        let result = match &config.auth {
            AuthMethod::AppPassword { token } => {
                if token.starts_with("fmu1-") {
                    Self::connect_with_token(&config.jmap_url, token).await
                } else {
                    Self::connect_with_basic(&config.jmap_url, &config.username, token).await
                }
            }
            AuthMethod::OAuth {
                access_token,
                token_endpoint,
                client_id,
                refresh_token,
                resource,
                ..
            } => {
                // If we have a cached access token, try it first
                if let Some(token) = access_token.as_deref() {
                    match Self::connect_with_token(&config.jmap_url, token).await {
                        Ok(result) => return Self::maybe_heal_capabilities(config, result),
                        Err(e) => {
                            log::info!("Cached OAuth access token failed ({}), refreshing", e);
                        }
                    }
                }

                // Read fresh refresh token from keyring (handles ratcheting across reconnects)
                let fresh_refresh_token = crate::keyring::get_oauth_refresh(&config.id)
                    .unwrap_or_else(|_| refresh_token.clone());

                // Refresh to get a new access token
                log::info!("Refreshing OAuth token for account {}", config.id);
                let token_set = neverlight_mail_oauth::refresh_access_token(
                    token_endpoint,
                    client_id,
                    &fresh_refresh_token,
                    "urn:ietf:params:oauth:scope:mail",
                    resource,
                )
                .await
                .map_err(|e| format!("OAuth token refresh failed: {e}"))?;

                // Persist new refresh token (servers use ratcheting — old token is invalidated)
                if let Some(ref new_refresh) = token_set.refresh_token {
                    if let Err(e) = crate::keyring::set_oauth_refresh(&config.id, new_refresh) {
                        log::warn!("Failed to persist refreshed OAuth token to keyring: {e}");
                        // Also update plaintext fallback in config file
                        if let Ok(Some(mut multi)) = crate::config::MultiAccountFileConfig::load() {
                            if let Some(fac) = multi.accounts.iter_mut().find(|a| a.id == config.id) {
                                if let crate::config::AuthBackend::OAuth { refresh_token_plaintext, .. } = &mut fac.auth {
                                    *refresh_token_plaintext = Some(new_refresh.clone());
                                }
                                let _ = multi.save();
                            }
                        }
                    }
                }

                Self::connect_with_token(&config.jmap_url, &token_set.access_token).await
            }
        };

        match result {
            Ok(pair) => Self::maybe_heal_capabilities(config, pair),
            Err(e) => Err(e),
        }
    }

    /// Compare discovered capabilities against config; update the file if stale.
    fn maybe_heal_capabilities(
        config: &AccountConfig,
        pair: (Self, JmapClient),
    ) -> Result<(Self, JmapClient), String> {
        let (ref session, _) = pair;
        let stale_push = config.capabilities.supports_push != session.supports_push;
        let stale_submission = config.capabilities.supports_submission != session.supports_submission;

        if stale_push || stale_submission {
            log::info!(
                "Healing capabilities for account {}: push {}→{}, submission {}→{}",
                config.id,
                config.capabilities.supports_push, session.supports_push,
                config.capabilities.supports_submission, session.supports_submission,
            );
            if let Ok(Some(mut multi)) = crate::config::MultiAccountFileConfig::load() {
                if let Some(fac) = multi.accounts.iter_mut().find(|a| a.id == config.id) {
                    fac.capabilities.supports_push = session.supports_push;
                    fac.capabilities.supports_submission = session.supports_submission;
                    if let Err(e) = multi.save() {
                        log::warn!("Failed to save healed capabilities: {e}");
                    }
                }
            }
        }

        Ok(pair)
    }

    /// Connect using a bearer token (e.g. Fastmail API token with `fmu1-` prefix).
    pub async fn connect_with_token(session_url: &str, token: &str) -> Result<(Self, JmapClient), String> {
        let auth = format!("Bearer {token}");
        Self::connect_with_auth(session_url, &auth).await
    }

    /// Connect using basic auth (e.g. Fastmail app password with `mu1-` prefix).
    pub async fn connect_with_basic(session_url: &str, username: &str, password: &str) -> Result<(Self, JmapClient), String> {
        let auth = basic_auth(username, password);
        Self::connect_with_auth(session_url, &auth).await
    }

    async fn connect_with_auth(session_url: &str, auth: &str) -> Result<(Self, JmapClient), String> {
        let http = reqwest::Client::new();
        let resp = http
            .get(session_url)
            .header("Authorization", auth)
            .send()
            .await
            .map_err(|e| format!("JMAP session fetch failed: {e}"))?;

        if !resp.status().is_success() {
            return Err(format!("JMAP session HTTP {}", resp.status()));
        }

        let body: serde_json::Value = resp
            .json()
            .await
            .map_err(|e| format!("JMAP session parse error: {e}"))?;

        let session = Self::parse(&body)?;
        let client = JmapClient::new(
            session.api_url.clone(),
            session.upload_url.clone(),
            session.download_url.clone(),
            session.event_source_url.clone(),
            session.account_id.clone(),
            auth.to_string(),
        ).map_err(|e| format!("Failed to build JMAP client: {e}"))?;

        Ok((session, client))
    }

    fn parse(json: &serde_json::Value) -> Result<Self, String> {
        let capabilities = json
            .get("capabilities")
            .and_then(|v| v.as_object())
            .ok_or("Missing capabilities in session")?;

        if !capabilities.contains_key("urn:ietf:params:jmap:mail") {
            return Err("Server does not support urn:ietf:params:jmap:mail".into());
        }

        let api_url = json
            .get("apiUrl")
            .and_then(|v| v.as_str())
            .ok_or("Missing apiUrl")?
            .to_string();

        let upload_url = json
            .get("uploadUrl")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();

        let download_url = json
            .get("downloadUrl")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();

        let event_source_url = json
            .get("eventSourceUrl")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());

        // Find primary mail account: must have urn:ietf:params:jmap:mail in
        // accountCapabilities. Fastmail sessions include non-mail accounts
        // (contacts, calendar) that will reject Mailbox/get calls.
        let accounts = json
            .get("accounts")
            .and_then(|v| v.as_object())
            .ok_or("Missing accounts in session")?;

        let has_mail_capability = |v: &serde_json::Value| -> bool {
            v.get("accountCapabilities")
                .and_then(|c| c.as_object())
                .is_some_and(|caps| caps.contains_key("urn:ietf:params:jmap:mail"))
        };

        let (account_id, _) = accounts
            .iter()
            .find(|(_, v)| {
                has_mail_capability(v)
                    && v.get("isPersonal")
                        .and_then(|p| p.as_bool())
                        .unwrap_or(false)
            })
            .or_else(|| accounts.iter().find(|(_, v)| has_mail_capability(v)))
            .or_else(|| accounts.iter().next())
            .ok_or("No accounts in session")?;

        let state = json
            .get("state")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();

        // Extract core capability limits
        let core_caps = capabilities
            .get("urn:ietf:params:jmap:core")
            .and_then(|v| v.as_object());

        let max_objects_in_get = core_caps
            .and_then(|c| c.get("maxObjectsInGet"))
            .and_then(|v| v.as_u64())
            .unwrap_or(500) as u32;

        let max_objects_in_set = core_caps
            .and_then(|c| c.get("maxObjectsInSet"))
            .and_then(|v| v.as_u64())
            .unwrap_or(500) as u32;

        let max_calls_in_request = core_caps
            .and_then(|c| c.get("maxCallsInRequest"))
            .and_then(|v| v.as_u64())
            .unwrap_or(16) as u32;

        let supports_push = capabilities.contains_key("urn:ietf:params:jmap:websocket")
            || event_source_url.is_some();
        let supports_submission =
            capabilities.contains_key("urn:ietf:params:jmap:submission");

        Ok(JmapSession {
            api_url,
            upload_url,
            download_url,
            event_source_url,
            account_id: account_id.clone(),
            state,
            max_objects_in_get,
            max_objects_in_set,
            max_calls_in_request,
            supports_push,
            supports_submission,
        })
    }
}

fn basic_auth(username: &str, password: &str) -> String {
    use std::io::Write;
    let mut buf = Vec::new();
    write!(buf, "{}:{}", username, password).unwrap();
    format!("Basic {}", base64_encode(&buf))
}

fn base64_encode(data: &[u8]) -> String {
    const ALPHABET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut result = String::new();
    for chunk in data.chunks(3) {
        let b0 = chunk[0] as u32;
        let b1 = chunk.get(1).map(|&b| b as u32).unwrap_or(0);
        let b2 = chunk.get(2).map(|&b| b as u32).unwrap_or(0);
        let triple = (b0 << 16) | (b1 << 8) | b2;
        result.push(ALPHABET[((triple >> 18) & 0x3F) as usize] as char);
        result.push(ALPHABET[((triple >> 12) & 0x3F) as usize] as char);
        if chunk.len() > 1 {
            result.push(ALPHABET[((triple >> 6) & 0x3F) as usize] as char);
        } else {
            result.push('=');
        }
        if chunk.len() > 2 {
            result.push(ALPHABET[(triple & 0x3F) as usize] as char);
        } else {
            result.push('=');
        }
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_fastmail_session() {
        let json = serde_json::json!({
            "capabilities": {
                "urn:ietf:params:jmap:core": {
                    "maxObjectsInGet": 1000,
                    "maxObjectsInSet": 500,
                    "maxCallsInRequest": 64
                },
                "urn:ietf:params:jmap:mail": {},
                "urn:ietf:params:jmap:submission": {}
            },
            "accounts": {
                "u1234": {
                    "name": "test@fastmail.com",
                    "isPersonal": true,
                    "accountCapabilities": {
                        "urn:ietf:params:jmap:mail": {}
                    }
                }
            },
            "apiUrl": "https://api.fastmail.com/jmap/api/",
            "uploadUrl": "https://api.fastmail.com/jmap/upload/{accountId}/",
            "downloadUrl": "https://api.fastmail.com/jmap/download/{accountId}/{blobId}/{name}?type={type}",
            "eventSourceUrl": "https://api.fastmail.com/jmap/event/",
            "state": "sess001"
        });

        let session = JmapSession::parse(&json).unwrap();
        assert_eq!(session.account_id, "u1234");
        assert_eq!(session.api_url, "https://api.fastmail.com/jmap/api/");
        assert_eq!(session.max_objects_in_get, 1000);
        assert_eq!(session.max_calls_in_request, 64);
        assert!(session.event_source_url.is_some());
        assert!(session.supports_push);
        assert!(session.supports_submission);
    }

    #[test]
    fn picks_mail_capable_account_over_non_mail() {
        let json = serde_json::json!({
            "capabilities": {
                "urn:ietf:params:jmap:core": {},
                "urn:ietf:params:jmap:mail": {}
            },
            "accounts": {
                "u-contacts": {
                    "name": "contacts",
                    "isPersonal": true,
                    "accountCapabilities": {
                        "urn:ietf:params:jmap:contacts": {}
                    }
                },
                "u-mail": {
                    "name": "mail",
                    "isPersonal": true,
                    "accountCapabilities": {
                        "urn:ietf:params:jmap:mail": {},
                        "urn:ietf:params:jmap:submission": {}
                    }
                }
            },
            "apiUrl": "https://api.example.com/jmap/",
            "state": "s1"
        });

        let session = JmapSession::parse(&json).unwrap();
        assert_eq!(session.account_id, "u-mail");
    }

    #[test]
    fn rejects_session_without_mail_capability() {
        let json = serde_json::json!({
            "capabilities": {
                "urn:ietf:params:jmap:core": {}
            },
            "accounts": {"u1": {"isPersonal": true}},
            "apiUrl": "https://example.com/jmap/"
        });
        assert!(JmapSession::parse(&json).is_err());
    }

    #[test]
    fn basic_auth_encoding() {
        let auth = basic_auth("user", "pass");
        assert_eq!(auth, "Basic dXNlcjpwYXNz");
    }
}
