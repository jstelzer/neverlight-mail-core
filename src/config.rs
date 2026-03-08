use serde::{Deserialize, Serialize};
use std::fs;
use std::path::PathBuf;

use crate::keyring;

// ---------------------------------------------------------------------------
// AccountId — stable UUIDv4 per account
// ---------------------------------------------------------------------------

pub type AccountId = String;

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
    /// How the auth token is stored.
    pub auth_token: PasswordBackend,
    #[serde(default)]
    pub email_addresses: Vec<String>,
    #[serde(default)]
    pub capabilities: AccountCapabilities,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "backend")]
pub enum PasswordBackend {
    #[serde(rename = "keyring")]
    Keyring,
    #[serde(rename = "plaintext")]
    Plaintext { value: String },
}

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

#[derive(Debug, Clone)]
pub struct AccountConfig {
    pub id: AccountId,
    pub label: String,
    /// JMAP session URL.
    pub jmap_url: String,
    pub username: String,
    /// Resolved auth token (bearer token or app password).
    pub token: String,
    pub email_addresses: Vec<String>,
    pub capabilities: AccountCapabilities,
}

impl AccountConfig {
    /// Build an AccountConfig from a FileAccountConfig + resolved token.
    pub fn from_file_account(fac: &FileAccountConfig, token: String) -> Self {
        AccountConfig {
            id: fac.id.clone(),
            label: fac.label.clone(),
            jmap_url: fac.jmap_url.clone(),
            username: fac.username.clone(),
            token,
            email_addresses: fac.email_addresses.clone(),
            capabilities: fac.capabilities.clone(),
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
                match resolve_token(&fac.auth_token, &fac.username, &fac.jmap_url) {
                    Ok(token) => {
                        accounts.push(AccountConfig::from_file_account(fac, token));
                    }
                    Err(e) => {
                        log::warn!(
                            "Failed to resolve token for account '{}': {}",
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
        .unwrap_or_else(|_| "https://api.fastmail.com/jmap/session".into());

    Some(AccountConfig {
        id: ENV_ACCOUNT_ID.to_string(),
        label: username.clone(),
        jmap_url,
        username,
        token,
        email_addresses: Vec::new(),
        capabilities: AccountCapabilities::default(),
    })
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn resolve_token(
    backend: &PasswordBackend,
    username: &str,
    jmap_url: &str,
) -> Result<String, String> {
    match backend {
        PasswordBackend::Plaintext { value } => Ok(value.clone()),
        PasswordBackend::Keyring => keyring::get_password(username, jmap_url),
    }
}
