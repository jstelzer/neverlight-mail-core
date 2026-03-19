//! UI-agnostic setup state machine.
//!
//! Both TUI and GUI frontends render this model and map their input events
//! to [`SetupInput`]. Validation, field navigation, and config persistence
//! live here so bugs are fixed once.
//!
//! JMAP-only: no IMAP/SMTP fields.

use crate::config::{
    new_account_id, AccountCapabilities, AccountId, ConfigNeedsInput, FileAccountConfig,
    MultiAccountFileConfig, PasswordBackend, DEFAULT_JMAP_SESSION_URL,
};
use crate::keyring;

// ---------------------------------------------------------------------------
// Field identity
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FieldId {
    Label,
    JmapUrl,
    Username,
    Token,
    Email,
}

impl FieldId {
    /// All fields in tab order for full setup.
    pub const FULL: &[FieldId] = &[
        Self::Label,
        Self::JmapUrl,
        Self::Username,
        Self::Token,
        Self::Email,
    ];

    /// Editable fields in token-only mode.
    pub const TOKEN_ONLY: &[FieldId] = &[Self::Token];

    /// Whether this field holds secret content (render masked).
    pub fn is_secret(self) -> bool {
        matches!(self, Self::Token)
    }

    /// Whether this field is a boolean toggle rather than text.
    pub fn is_toggle(self) -> bool {
        false
    }
}

// ---------------------------------------------------------------------------
// Setup request (what the engine needs)
// ---------------------------------------------------------------------------

/// What prompted the setup — drives which fields are editable.
#[derive(Debug, Clone)]
pub enum SetupRequest {
    /// No config file exists — full account creation.
    Full,
    /// Config exists but token can't be resolved.
    TokenOnly {
        account_id: AccountId,
        jmap_url: String,
        username: String,
        reason: Option<String>,
    },
    /// Editing an existing account (all fields, pre-filled).
    Edit { account_id: AccountId },
    /// OAuth refresh token is stale — need browser re-auth.
    /// No fields are editable; the UI should show account info (read-only)
    /// and an "Authorize with browser" button.
    Reauth {
        account_id: AccountId,
        jmap_url: String,
        username: String,
        reason: String,
    },
}

impl SetupRequest {
    /// Build from the error returned by `resolve_all_accounts()`.
    pub fn from_config_needs(needs: &ConfigNeedsInput) -> Self {
        match needs {
            ConfigNeedsInput::FullSetup => Self::Full,
            ConfigNeedsInput::TokenOnly {
                account_id,
                jmap_url,
                username,
                error,
            } => Self::TokenOnly {
                account_id: account_id.clone(),
                jmap_url: jmap_url.clone(),
                username: username.clone(),
                reason: error.clone(),
            },
            ConfigNeedsInput::OAuthReauth {
                account_id,
                jmap_url,
                username,
                error,
                ..
            } => Self::Reauth {
                account_id: account_id.clone(),
                jmap_url: jmap_url.clone(),
                username: username.clone(),
                reason: error.clone(),
            },
        }
    }

    /// Which fields the operator can edit.
    pub fn editable_fields(&self) -> &[FieldId] {
        match self {
            Self::Full | Self::Edit { .. } => FieldId::FULL,
            Self::TokenOnly { .. } => FieldId::TOKEN_ONLY,
            Self::Reauth { .. } => &[], // No editable fields — OAuth browser flow only
        }
    }

    /// Whether a given field is read-only in this request mode.
    pub fn is_readonly(&self, field: FieldId) -> bool {
        !self.editable_fields().contains(&field)
    }
}

// ---------------------------------------------------------------------------
// Setup model (the state machine)
// ---------------------------------------------------------------------------

/// UI-agnostic setup state. Frontends read fields for rendering and call
/// [`update()`] with mapped input events.
pub struct SetupModel {
    pub request: SetupRequest,
    pub label: String,
    pub jmap_url: String,
    pub username: String,
    pub token: String,
    pub email: String,
    pub active_field: FieldId,
    pub error: Option<String>,
}

impl SetupModel {
    /// Create a new setup model from a [`ConfigNeedsInput`] error.
    pub fn from_config_needs(needs: &ConfigNeedsInput) -> Self {
        let request = SetupRequest::from_config_needs(needs);
        match needs {
            ConfigNeedsInput::FullSetup => Self {
                request,
                label: String::new(),
                jmap_url: DEFAULT_JMAP_SESSION_URL.into(),
                username: String::new(),
                token: String::new(),
                email: String::new(),
                active_field: FieldId::JmapUrl,
                error: None,
            },
            ConfigNeedsInput::TokenOnly {
                jmap_url,
                username,
                error,
                ..
            } => Self {
                request,
                label: String::new(),
                jmap_url: jmap_url.clone(),
                username: username.clone(),
                token: String::new(),
                email: String::new(),
                active_field: FieldId::Token,
                error: error.clone(),
            },
            ConfigNeedsInput::OAuthReauth {
                jmap_url,
                username,
                label,
                error,
                ..
            } => Self {
                request,
                label: label.clone(),
                jmap_url: jmap_url.clone(),
                username: username.clone(),
                token: String::new(),
                email: String::new(),
                active_field: FieldId::Token, // no editable fields, but need a default
                error: Some(error.clone()),
            },
        }
    }

    /// Create a setup model for editing an existing account.
    /// Token is intentionally left empty (must be re-entered).
    pub fn for_edit(account_id: AccountId, fields: SetupFields) -> Self {
        Self {
            request: SetupRequest::Edit { account_id },
            label: fields.label,
            jmap_url: fields.jmap_url,
            username: fields.username,
            token: String::new(),
            email: fields.email,
            active_field: FieldId::JmapUrl,
            error: None,
        }
    }

    /// Whether this setup requires an OAuth browser re-auth flow.
    /// Frontends should show an "Authorize with browser" button instead of
    /// (or in addition to) the normal form fields.
    pub fn is_reauth(&self) -> bool {
        matches!(self.request, SetupRequest::Reauth { .. })
    }

    /// The account ID for Reauth/TokenOnly/Edit requests.
    pub fn account_id(&self) -> Option<&str> {
        match &self.request {
            SetupRequest::Reauth { account_id, .. }
            | SetupRequest::TokenOnly { account_id, .. }
            | SetupRequest::Edit { account_id } => Some(account_id),
            SetupRequest::Full => None,
        }
    }

    /// Title string for the setup dialog/form.
    pub fn title(&self) -> &str {
        match &self.request {
            SetupRequest::Full => "Account Setup",
            SetupRequest::TokenOnly { .. } => "Enter API Token",
            SetupRequest::Edit { .. } => "Edit Account",
            SetupRequest::Reauth { .. } => "Re-authorize Account",
        }
    }

    /// Whether a specific field is read-only.
    pub fn is_readonly(&self, field: FieldId) -> bool {
        self.request.is_readonly(field)
    }

    /// Get the current value of a text field.
    pub fn field_value(&self, field: FieldId) -> &str {
        match field {
            FieldId::Label => &self.label,
            FieldId::JmapUrl => &self.jmap_url,
            FieldId::Username => &self.username,
            FieldId::Token => &self.token,
            FieldId::Email => &self.email,
        }
    }

    /// Mutable reference to a text field (None if readonly).
    fn field_mut(&mut self, field: FieldId) -> Option<&mut String> {
        if self.is_readonly(field) {
            return None;
        }
        match field {
            FieldId::Label => Some(&mut self.label),
            FieldId::JmapUrl => Some(&mut self.jmap_url),
            FieldId::Username => Some(&mut self.username),
            FieldId::Token => Some(&mut self.token),
            FieldId::Email => Some(&mut self.email),
        }
    }

    /// Process an input event. Returns what the UI should do next.
    pub fn update(&mut self, input: SetupInput) -> SetupTransition {
        match input {
            SetupInput::NextField => self.cycle_field(1),
            SetupInput::PrevField => self.cycle_field(-1),
            SetupInput::SetField(field, value) => {
                if !self.is_readonly(field) {
                    match field {
                        FieldId::Label => self.label = value,
                        FieldId::JmapUrl => self.jmap_url = value,
                        FieldId::Username => self.username = value,
                        FieldId::Token => self.token = value,
                        FieldId::Email => self.email = value,
                    }
                    self.error = None;
                }
            }
            SetupInput::InsertChar(c) => {
                if let Some(f) = self.field_mut(self.active_field) {
                    f.push(c);
                    self.error = None;
                }
            }
            SetupInput::Backspace => {
                if let Some(f) = self.field_mut(self.active_field) {
                    f.pop();
                }
            }
            SetupInput::Submit => {
                return self.try_submit();
            }
            SetupInput::Cancel => {
                return SetupTransition::Finished(SetupOutcome::Cancelled);
            }
        }
        SetupTransition::Continue
    }

    fn cycle_field(&mut self, direction: i32) {
        let fields = self.request.editable_fields();
        if fields.len() <= 1 {
            return;
        }
        if let Some(idx) = fields.iter().position(|&f| f == self.active_field) {
            let len = fields.len() as i32;
            let next = ((idx as i32 + direction).rem_euclid(len)) as usize;
            self.active_field = fields[next];
        }
    }

    fn try_submit(&mut self) -> SetupTransition {
        match &self.request {
            SetupRequest::TokenOnly {
                account_id,
                jmap_url,
                username,
                ..
            } => {
                if self.token.is_empty() {
                    self.error = Some("API token is required".into());
                    return SetupTransition::Continue;
                }

                let token_backend = store_token(username, jmap_url, &self.token);

                let mut multi = match MultiAccountFileConfig::load() {
                    Ok(Some(m)) => m,
                    _ => {
                        self.error = Some("Could not load existing config".into());
                        return SetupTransition::Continue;
                    }
                };
                match multi.accounts.iter_mut().find(|a| a.id == *account_id) {
                    Some(acct) => acct.auth = token_backend,
                    None => {
                        self.error = Some("Account not found in config".into());
                        return SetupTransition::Continue;
                    }
                }
                if let Err(e) = multi.save() {
                    self.error = Some(format!("Failed to save config: {e}"));
                    return SetupTransition::Continue;
                }
                SetupTransition::Finished(SetupOutcome::Configured)
            }

            SetupRequest::Full => {
                if let Some(err) = self.validate() {
                    self.error = Some(err);
                    return SetupTransition::Continue;
                }

                let jmap_url = self.jmap_url.trim().to_string();
                let username = self.username.trim().to_string();
                let email_addresses = parse_email_list(&self.email);
                let label = if self.label.trim().is_empty() {
                    username.clone()
                } else {
                    self.label.trim().to_string()
                };
                let account_id = new_account_id();

                let token_backend = store_token(&username, &jmap_url, &self.token);

                let fac = FileAccountConfig {
                    id: account_id,
                    label,
                    jmap_url,
                    username,
                    auth: token_backend,
                    email_addresses,
                    capabilities: AccountCapabilities::default(),
                    max_messages_per_mailbox: None,
                };

                let mut multi = MultiAccountFileConfig::load().ok().flatten().unwrap_or(
                    MultiAccountFileConfig {
                        accounts: Vec::new(),
                    },
                );
                if multi
                    .accounts
                    .iter()
                    .any(|a| a.jmap_url == fac.jmap_url && a.username == fac.username)
                {
                    self.error = Some("Account already exists for this server/username".into());
                    return SetupTransition::Continue;
                }
                multi.accounts.push(fac);
                if let Err(e) = multi.save() {
                    self.error = Some(format!("Failed to save config: {e}"));
                    return SetupTransition::Continue;
                }
                SetupTransition::Finished(SetupOutcome::Configured)
            }

            SetupRequest::Edit { account_id } => {
                if let Some(err) = self.validate_edit() {
                    self.error = Some(err);
                    return SetupTransition::Continue;
                }

                let jmap_url = self.jmap_url.trim().to_string();
                let username = self.username.trim().to_string();
                let email_addresses = parse_email_list(&self.email);
                let label = if self.label.trim().is_empty() {
                    username.clone()
                } else {
                    self.label.trim().to_string()
                };

                let mut multi = MultiAccountFileConfig::load().ok().flatten().unwrap_or(
                    MultiAccountFileConfig {
                        accounts: Vec::new(),
                    },
                );

                let existing = match multi.accounts.iter().find(|a| a.id == *account_id) {
                    Some(a) => a,
                    None => {
                        self.error = Some("Account not found in config".into());
                        return SetupTransition::Continue;
                    }
                };

                // If URL/username changed, require token re-entry
                let creds_changed = existing.jmap_url != jmap_url || existing.username != username;
                if creds_changed && self.token.is_empty() {
                    self.error = Some("Token required when changing server URL or username".into());
                    return SetupTransition::Continue;
                }

                let token_backend = if self.token.is_empty() {
                    existing.auth.clone()
                } else {
                    store_token(&username, &jmap_url, &self.token)
                };

                let fac = FileAccountConfig {
                    id: account_id.clone(),
                    label,
                    jmap_url,
                    username,
                    auth: token_backend,
                    email_addresses,
                    capabilities: existing.capabilities.clone(),
                    max_messages_per_mailbox: existing.max_messages_per_mailbox,
                };

                if let Some(pos) = multi.accounts.iter().position(|a| a.id == *account_id) {
                    multi.accounts[pos] = fac;
                } else {
                    multi.accounts.push(fac);
                }
                if let Err(e) = multi.save() {
                    self.error = Some(format!("Failed to save config: {e}"));
                    return SetupTransition::Continue;
                }
                SetupTransition::Finished(SetupOutcome::Configured)
            }

            SetupRequest::Reauth { .. } => {
                // Reauth is driven by the frontend's OAuth flow, not the form.
                // Submit is a no-op; the frontend should trigger OAuth and call
                // `complete_reauth()` with the new tokens.
                self.error = Some("Use the browser sign-in button to re-authorize".into());
                SetupTransition::Continue
            }
        }
    }

    /// Validate the current fields. Returns `None` if valid, `Some(error)` if not.
    pub fn validate(&self) -> Option<String> {
        if let Some(e) = validate_jmap_url(&self.jmap_url) {
            return Some(e);
        }
        if self.username.trim().is_empty() {
            return Some("Username is required".into());
        }
        if self.token.is_empty() {
            return Some("API token is required".into());
        }
        if parse_email_list(&self.email).is_empty() {
            return Some("At least one email address is required".into());
        }
        None
    }

    /// Edit validation: token is optional (empty = keep existing).
    fn validate_edit(&self) -> Option<String> {
        if let Some(e) = validate_jmap_url(&self.jmap_url) {
            return Some(e);
        }
        if self.username.trim().is_empty() {
            return Some("Username is required".into());
        }
        if parse_email_list(&self.email).is_empty() {
            return Some("At least one email address is required".into());
        }
        None
    }
}

// ---------------------------------------------------------------------------
// Input / output types
// ---------------------------------------------------------------------------

/// Input events the UI maps its native events to.
#[derive(Debug, Clone)]
pub enum SetupInput {
    /// Tab / move to next editable field.
    NextField,
    /// Shift-Tab / move to previous editable field.
    PrevField,
    /// Set a field's entire value (for widget-based UIs like COSMIC).
    SetField(FieldId, String),
    /// Insert a character at cursor (for keystroke-based UIs like TUI).
    InsertChar(char),
    /// Delete last character from active text field.
    Backspace,
    /// Attempt to save and exit.
    Submit,
    /// Abort setup.
    Cancel,
}

/// What the UI should do after processing input.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SetupTransition {
    /// Keep showing the form.
    Continue,
    /// Setup is done — UI should exit the form.
    Finished(SetupOutcome),
}

/// Final result of the setup flow.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SetupOutcome {
    /// Config was written. Re-resolve accounts and proceed.
    Configured,
    /// Operator cancelled. Exit gracefully.
    Cancelled,
}

/// Pre-filled field values for the Edit flow.
pub struct SetupFields {
    pub label: String,
    pub jmap_url: String,
    pub username: String,
    pub email: String,
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Check whether a connect error indicates an OAuth ratchet/grant failure
/// that requires browser re-authorization.
pub fn is_oauth_reauth_error(error: &str) -> bool {
    error.contains("invalid_grant") || error.contains("OAuth token refresh failed")
}

/// Store a token in keyring, fall back to plaintext.
pub fn store_token(username: &str, jmap_url: &str, token: &str) -> PasswordBackend {
    match keyring::set_password(username, jmap_url, token) {
        Ok(()) => {
            log::info!("Token stored in keyring for {}@{}", username, jmap_url);
            PasswordBackend::Keyring
        }
        Err(e) => {
            log::warn!("Keyring unavailable ({}), using plaintext", e);
            PasswordBackend::Plaintext {
                value: token.to_string(),
            }
        }
    }
}

/// Lightweight JMAP URL sanity check.
fn validate_jmap_url(url: &str) -> Option<String> {
    let url = url.trim();
    if url.is_empty() {
        return Some("JMAP session URL is required".into());
    }
    if !url.starts_with("https://") {
        return Some("JMAP URL must start with https://".into());
    }
    if url.len() <= "https://".len() {
        return Some("JMAP URL is incomplete".into());
    }
    None
}

/// Split a comma-separated email string into a list, trimming whitespace
/// and dropping empty entries.
fn parse_email_list(input: &str) -> Vec<String> {
    input
        .split(',')
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect()
}
