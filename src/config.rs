use serde::{Deserialize, Serialize};
use std::fs;
use std::path::PathBuf;

use crate::keyring;

// ---------------------------------------------------------------------------
// AccountId — stable UUIDv4 per account
// ---------------------------------------------------------------------------

pub type AccountId = String;

/// Default JMAP session URL used when no server URL is configured.
/// Fastmail is the default provider; frontends should let users override this.
pub const DEFAULT_JMAP_SESSION_URL: &str = "https://api.fastmail.com/jmap/session";

pub fn new_account_id() -> AccountId {
    uuid::Uuid::new_v4().to_string()
}

/// Synthetic ID for env-var-based accounts (stable across restarts).
pub const ENV_ACCOUNT_ID: &str = "env-account";

// ---------------------------------------------------------------------------
// Capabilities discovered during account setup
// ---------------------------------------------------------------------------

/// Capabilities discovered during account setup.
/// Stored alongside the account so we don't need to re-probe on every launch.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct AccountCapabilities {
    /// Cached JMAP session resource URL (from `.well-known/jmap` discovery).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub jmap_session_url: Option<String>,
    /// Whether the server advertises JMAP push (EventSource).
    #[serde(default)]
    pub supports_push: bool,
    /// Whether the server advertises JMAP EmailSubmission.
    #[serde(default)]
    pub supports_submission: bool,
}

// ---------------------------------------------------------------------------
// On-disk per-account config
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileAccountConfig {
    pub id: AccountId,
    pub label: String,
    /// JMAP session URL (e.g. "https://api.fastmail.com/jmap/session").
    pub jmap_url: String,
    pub username: String,
    /// How auth credentials are stored.
    /// Uses `auth` for new OAuth-aware format, falls back to `auth_token` for legacy.
    #[serde(alias = "auth_token")]
    pub auth: AuthBackend,
    #[serde(default)]
    pub email_addresses: Vec<String>,
    #[serde(default)]
    pub capabilities: AccountCapabilities,
    /// Maximum messages to backfill per mailbox. None = unlimited.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_messages_per_mailbox: Option<u32>,
}

/// On-disk auth credential storage. Backward-compatible: existing configs with
/// `"backend": "keyring"` or `"backend": "plaintext"` deserialize unchanged.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "backend")]
pub enum AuthBackend {
    /// App password stored in OS keyring.
    #[serde(rename = "keyring")]
    Keyring,
    /// App password stored as plaintext (keyring fallback).
    #[serde(rename = "plaintext")]
    Plaintext { value: String },
    /// OAuth 2.0 credentials. Refresh token in keyring, client_id in config.
    #[serde(rename = "oauth")]
    OAuth {
        issuer: String,
        client_id: String,
        resource: String,
        token_endpoint: String,
        /// Plaintext fallback for refresh token (when keyring unavailable).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        refresh_token_plaintext: Option<String>,
    },
}

/// Legacy alias for backward compatibility in setup.rs and other call sites.
pub type PasswordBackend = AuthBackend;

// ---------------------------------------------------------------------------
// Multi-account file config
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MultiAccountFileConfig {
    pub accounts: Vec<FileAccountConfig>,
}

// ---------------------------------------------------------------------------
// Runtime account config (resolved tokens, ready to use)
// ---------------------------------------------------------------------------

/// Runtime auth method — resolved from config + keyring at startup.
#[derive(Debug, Clone)]
pub enum AuthMethod {
    /// App password or bearer token (existing behavior).
    AppPassword { token: String },
    /// OAuth 2.0 with PKCE.
    OAuth {
        issuer: String,
        client_id: String,
        token_endpoint: String,
        refresh_token: String,
        /// Cached access token (short-lived, may be expired).
        access_token: Option<String>,
        /// Resource URL used during authorization.
        resource: String,
    },
}

#[derive(Debug, Clone)]
pub struct AccountConfig {
    pub id: AccountId,
    pub label: String,
    /// JMAP session URL.
    pub jmap_url: String,
    pub username: String,
    /// Resolved auth credentials.
    pub auth: AuthMethod,
    pub email_addresses: Vec<String>,
    pub capabilities: AccountCapabilities,
    /// Maximum messages to backfill per mailbox. None = unlimited.
    pub max_messages_per_mailbox: Option<u32>,
}

impl AccountConfig {
    /// Convenience accessor for backward-compatible code paths.
    /// Returns the app password token, or the current access token for OAuth.
    pub fn token(&self) -> Option<&str> {
        match &self.auth {
            AuthMethod::AppPassword { token } => Some(token),
            AuthMethod::OAuth { access_token, .. } => access_token.as_deref(),
        }
    }

    /// Build an AccountConfig from a FileAccountConfig + resolved app-password token.
    pub fn from_file_account(fac: &FileAccountConfig, token: String) -> Self {
        AccountConfig {
            id: fac.id.clone(),
            label: fac.label.clone(),
            jmap_url: fac.jmap_url.clone(),
            username: fac.username.clone(),
            auth: AuthMethod::AppPassword { token },
            email_addresses: fac.email_addresses.clone(),
            capabilities: fac.capabilities.clone(),
            max_messages_per_mailbox: fac.max_messages_per_mailbox,
        }
    }

    /// Build an AccountConfig from a FileAccountConfig + resolved OAuth credentials.
    pub fn from_file_account_oauth(
        fac: &FileAccountConfig,
        issuer: String,
        client_id: String,
        token_endpoint: String,
        refresh_token: String,
        resource: String,
    ) -> Self {
        AccountConfig {
            id: fac.id.clone(),
            label: fac.label.clone(),
            jmap_url: fac.jmap_url.clone(),
            username: fac.username.clone(),
            auth: AuthMethod::OAuth {
                issuer,
                client_id,
                token_endpoint,
                refresh_token,
                access_token: None,
                resource,
            },
            email_addresses: fac.email_addresses.clone(),
            capabilities: fac.capabilities.clone(),
            max_messages_per_mailbox: fac.max_messages_per_mailbox,
        }
    }
}

// ---------------------------------------------------------------------------
// What the dialog needs to show when credentials can't be resolved
// ---------------------------------------------------------------------------

/// What the dialog needs to show when credentials can't be resolved automatically.
#[derive(Debug, Clone)]
pub enum ConfigNeedsInput {
    /// No config file exists — show full setup form.
    FullSetup,
    /// Config exists but token is missing from keyring.
    TokenOnly {
        account_id: AccountId,
        jmap_url: String,
        username: String,
        error: Option<String>,
    },
    /// OAuth refresh token is stale — need to redo the browser auth flow.
    /// Unlike `TokenOnly`, the user can't paste a token; they must go through
    /// the OAuth browser redirect again.
    OAuthReauth {
        account_id: AccountId,
        label: String,
        jmap_url: String,
        username: String,
        error: String,
    },
}

// ---------------------------------------------------------------------------
// File paths
// ---------------------------------------------------------------------------

fn config_dir() -> PathBuf {
    dirs::config_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("neverlight-mail")
}

fn config_path() -> PathBuf {
    config_dir().join("config.json")
}

// ---------------------------------------------------------------------------
// Layout config (unchanged)
// ---------------------------------------------------------------------------

/// Persisted pane layout ratios.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LayoutConfig {
    /// Ratio of the outer split (sidebar vs rest). Default ~0.15.
    pub sidebar_ratio: f32,
    /// Ratio of the inner split (message list vs message view). Default ~0.40.
    pub list_ratio: f32,
}

impl Default for LayoutConfig {
    fn default() -> Self {
        Self {
            sidebar_ratio: 0.15,
            list_ratio: 0.40,
        }
    }
}

impl LayoutConfig {
    pub fn load() -> Self {
        let path = config_dir().join("layout.json");
        if let Ok(data) = fs::read_to_string(&path) {
            if let Ok(cfg) = serde_json::from_str::<LayoutConfig>(&data) {
                // Clamp to sane range
                return LayoutConfig {
                    sidebar_ratio: cfg.sidebar_ratio.clamp(0.05, 0.50),
                    list_ratio: cfg.list_ratio.clamp(0.15, 0.85),
                };
            }
        }
        Self::default()
    }

    pub fn save(&self) {
        let path = config_dir().join("layout.json");
        if let Some(parent) = path.parent() {
            let _ = fs::create_dir_all(parent);
        }
        if let Ok(data) = serde_json::to_string_pretty(self) {
            let _ = fs::write(&path, data);
        }
    }
}

// ---------------------------------------------------------------------------
// Multi-account config: load / save
// ---------------------------------------------------------------------------

impl MultiAccountFileConfig {
    pub fn load() -> Result<Option<Self>, String> {
        let path = config_path();
        if !path.exists() {
            return Ok(None);
        }
        let data = fs::read_to_string(&path).map_err(|e| format!("read config: {e}"))?;

        if let Ok(multi) = serde_json::from_str::<MultiAccountFileConfig>(&data) {
            return Ok(Some(multi));
        }

        Err("Failed to parse config file".into())
    }

    pub fn save(&self) -> Result<(), String> {
        let path = config_path();
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(|e| format!("create config dir: {e}"))?;
        }
        let data =
            serde_json::to_string_pretty(self).map_err(|e| format!("serialize config: {e}"))?;
        fs::write(&path, data).map_err(|e| format!("write config: {e}"))
    }
}

// ---------------------------------------------------------------------------
// Config resolution
// ---------------------------------------------------------------------------

/// Resolve all configured accounts.
///
/// Resolution order:
/// 1. Environment variables (`NEVERLIGHT_MAIL_JMAP_TOKEN` + `NEVERLIGHT_MAIL_USER`) → single env account
/// 2. Config file (`~/.config/neverlight-mail/config.json`) → multi-account with keyring
/// 3. Returns `Err(ConfigNeedsInput)` if UI input is needed
pub fn resolve_all_accounts() -> Result<Vec<AccountConfig>, ConfigNeedsInput> {
    // 1. Env vars → single env account (JMAP token + user)
    if let Some(account) = account_from_env() {
        log::info!("Config loaded from environment variables");
        return Ok(vec![account]);
    }

    // 2. Config file
    match MultiAccountFileConfig::load() {
        Ok(Some(multi)) => {
            let mut accounts = Vec::new();
            for fac in &multi.accounts {
                match resolve_account(fac) {
                    Ok(config) => {
                        accounts.push(config);
                    }
                    Err(e) => {
                        log::warn!(
                            "Failed to resolve credentials for account '{}': {}",
                            fac.label,
                            e
                        );
                    }
                }
            }
            if accounts.is_empty() && !multi.accounts.is_empty() {
                let fac = &multi.accounts[0];
                return Err(ConfigNeedsInput::TokenOnly {
                    account_id: fac.id.clone(),
                    jmap_url: fac.jmap_url.clone(),
                    username: fac.username.clone(),
                    error: Some("Keyring unavailable for all accounts".into()),
                });
            }
            if accounts.is_empty() {
                return Err(ConfigNeedsInput::FullSetup);
            }
            Ok(accounts)
        }
        Ok(None) => {
            log::info!("No config file found, need full setup");
            Err(ConfigNeedsInput::FullSetup)
        }
        Err(e) => {
            log::warn!("Config file error: {}", e);
            Err(ConfigNeedsInput::FullSetup)
        }
    }
}

/// Try to build an account from environment variables.
fn account_from_env() -> Option<AccountConfig> {
    let token = std::env::var("NEVERLIGHT_MAIL_JMAP_TOKEN").ok()?;
    let username = std::env::var("NEVERLIGHT_MAIL_USER").ok()?;
    let jmap_url = std::env::var("NEVERLIGHT_MAIL_JMAP_URL")
        .unwrap_or_else(|_| DEFAULT_JMAP_SESSION_URL.into());

    Some(AccountConfig {
        id: ENV_ACCOUNT_ID.to_string(),
        label: username.clone(),
        jmap_url,
        username,
        auth: AuthMethod::AppPassword { token },
        email_addresses: Vec::new(),
        capabilities: AccountCapabilities::default(),
        max_messages_per_mailbox: None,
    })
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Resolve a FileAccountConfig into a runtime AccountConfig.
fn resolve_account(fac: &FileAccountConfig) -> Result<AccountConfig, String> {
    match &fac.auth {
        AuthBackend::Plaintext { value } => {
            Ok(AccountConfig::from_file_account(fac, value.clone()))
        }
        AuthBackend::Keyring => {
            let token = keyring::get_password(&fac.username, &fac.jmap_url)?;
            Ok(AccountConfig::from_file_account(fac, token))
        }
        AuthBackend::OAuth {
            issuer,
            client_id,
            resource,
            token_endpoint,
            refresh_token_plaintext,
        } => {
            // Try keyring first, fall back to plaintext
            let refresh_token = keyring::get_oauth_refresh(&fac.id)
                .or_else(|_| {
                    refresh_token_plaintext
                        .clone()
                        .ok_or_else(|| "No refresh token in keyring or config".to_string())
                })?;
            Ok(AccountConfig::from_file_account_oauth(
                fac,
                issuer.clone(),
                client_id.clone(),
                token_endpoint.clone(),
                refresh_token,
                resource.clone(),
            ))
        }
    }
}

/// Resolve an app password token from an AuthBackend (legacy helper).
pub fn resolve_token(
    backend: &AuthBackend,
    username: &str,
    jmap_url: &str,
) -> Result<String, String> {
    match backend {
        AuthBackend::Plaintext { value } => Ok(value.clone()),
        AuthBackend::Keyring => keyring::get_password(username, jmap_url),
        AuthBackend::OAuth { .. } => {
            Err("Cannot resolve app password from OAuth backend".into())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn auth_backend_keyring_roundtrips() {
        let backend = AuthBackend::Keyring;
        let json = serde_json::to_string(&backend).unwrap();
        assert!(json.contains(r#""backend":"keyring""#));
        let parsed: AuthBackend = serde_json::from_str(&json).unwrap();
        assert!(matches!(parsed, AuthBackend::Keyring));
    }

    #[test]
    fn auth_backend_plaintext_roundtrips() {
        let backend = AuthBackend::Plaintext {
            value: "fmu1-secret".into(),
        };
        let json = serde_json::to_string(&backend).unwrap();
        let parsed: AuthBackend = serde_json::from_str(&json).unwrap();
        match parsed {
            AuthBackend::Plaintext { value } => assert_eq!(value, "fmu1-secret"),
            _ => panic!("expected Plaintext"),
        }
    }

    #[test]
    fn auth_backend_oauth_roundtrips() {
        let backend = AuthBackend::OAuth {
            issuer: "https://auth.fastmail.com".into(),
            client_id: "client-123".into(),
            resource: "https://api.fastmail.com/jmap/session".into(),
            token_endpoint: "https://auth.fastmail.com/token".into(),
            refresh_token_plaintext: None,
        };
        let json = serde_json::to_string(&backend).unwrap();
        assert!(json.contains(r#""backend":"oauth""#));
        let parsed: AuthBackend = serde_json::from_str(&json).unwrap();
        match parsed {
            AuthBackend::OAuth {
                issuer, client_id, ..
            } => {
                assert_eq!(issuer, "https://auth.fastmail.com");
                assert_eq!(client_id, "client-123");
            }
            _ => panic!("expected OAuth"),
        }
    }

    #[test]
    fn legacy_keyring_config_deserializes() {
        // Existing config files use "auth_token" field name — test that alias works
        let json = r#"{
            "id": "test-id",
            "label": "Test",
            "jmap_url": "https://api.fastmail.com/jmap/session",
            "username": "test@fastmail.com",
            "auth_token": {"backend": "keyring"},
            "email_addresses": ["test@fastmail.com"]
        }"#;
        let fac: FileAccountConfig = serde_json::from_str(json).unwrap();
        assert!(matches!(fac.auth, AuthBackend::Keyring));
    }

    #[test]
    fn new_auth_field_deserializes() {
        let json = r#"{
            "id": "test-id",
            "label": "Test",
            "jmap_url": "https://api.fastmail.com/jmap/session",
            "username": "test@fastmail.com",
            "auth": {"backend": "oauth", "issuer": "https://auth.example.com", "client_id": "c1", "resource": "https://api.example.com", "token_endpoint": "https://auth.example.com/token"},
            "email_addresses": ["test@fastmail.com"]
        }"#;
        let fac: FileAccountConfig = serde_json::from_str(json).unwrap();
        assert!(matches!(fac.auth, AuthBackend::OAuth { .. }));
    }

    #[test]
    fn config_round_trips_max_messages() {
        // With the field set
        let json = r#"{
            "id": "test-id",
            "label": "Test",
            "jmap_url": "https://api.fastmail.com/jmap/session",
            "username": "test@fastmail.com",
            "auth": {"backend": "keyring"},
            "email_addresses": [],
            "max_messages_per_mailbox": 5000
        }"#;
        let fac: FileAccountConfig = serde_json::from_str(json).unwrap();
        assert_eq!(fac.max_messages_per_mailbox, Some(5000));

        let serialized = serde_json::to_string(&fac).unwrap();
        let reparsed: FileAccountConfig = serde_json::from_str(&serialized).unwrap();
        assert_eq!(reparsed.max_messages_per_mailbox, Some(5000));

        // Without the field (defaults to None, omitted on serialization)
        let json_no_field = r#"{
            "id": "test-id",
            "label": "Test",
            "jmap_url": "https://api.fastmail.com/jmap/session",
            "username": "test@fastmail.com",
            "auth": {"backend": "keyring"},
            "email_addresses": []
        }"#;
        let fac2: FileAccountConfig = serde_json::from_str(json_no_field).unwrap();
        assert_eq!(fac2.max_messages_per_mailbox, None);

        let serialized2 = serde_json::to_string(&fac2).unwrap();
        assert!(!serialized2.contains("max_messages_per_mailbox"));
    }
}
