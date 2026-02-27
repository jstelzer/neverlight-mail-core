//! UI-agnostic setup state machine.
//!
//! Both TUI and GUI frontends render this model and map their input events
//! to [`SetupInput`]. Validation, field navigation, and config persistence
//! live here so bugs are fixed once.

use crate::config::{
    AccountId, ConfigNeedsInput, FileAccountConfig, MultiAccountFileConfig, PasswordBackend,
    SmtpOverrides, new_account_id,
};
use crate::keyring;

// ---------------------------------------------------------------------------
// Field identity
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FieldId {
    Label,
    Server,
    Port,
    Username,
    Password,
    Email,
    Starttls,
}

impl FieldId {
    /// All fields in tab order for full setup.
    pub const FULL: &[FieldId] = &[
        Self::Label,
        Self::Server,
        Self::Port,
        Self::Username,
        Self::Password,
        Self::Email,
        Self::Starttls,
    ];

    /// Editable fields in password-only mode.
    pub const PASSWORD_ONLY: &[FieldId] = &[Self::Password];

    /// Whether this field holds secret content (render masked).
    pub fn is_secret(self) -> bool {
        matches!(self, Self::Password)
    }

    /// Whether this field is a boolean toggle rather than text.
    pub fn is_toggle(self) -> bool {
        matches!(self, Self::Starttls)
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
    /// Config exists but password can't be resolved.
    PasswordOnly {
        account_id: AccountId,
        server: String,
        username: String,
        reason: Option<String>,
    },
    /// Editing an existing account (all fields, pre-filled).
    Edit { account_id: AccountId },
}

impl SetupRequest {
    /// Build from the error returned by `Config::resolve_all_accounts()`.
    pub fn from_config_needs(needs: &ConfigNeedsInput) -> Self {
        match needs {
            ConfigNeedsInput::FullSetup => Self::Full,
            ConfigNeedsInput::PasswordOnly {
                account_id,
                server,
                username,
                error,
                ..
            } => Self::PasswordOnly {
                account_id: account_id.clone(),
                server: server.clone(),
                username: username.clone(),
                reason: error.clone(),
            },
        }
    }

    /// Which fields the operator can edit.
    pub fn editable_fields(&self) -> &[FieldId] {
        match self {
            Self::Full | Self::Edit { .. } => FieldId::FULL,
            Self::PasswordOnly { .. } => FieldId::PASSWORD_ONLY,
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
    pub server: String,
    pub port: String,
    pub username: String,
    pub password: String,
    pub email: String,
    pub starttls: bool,
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
                server: String::new(),
                port: "993".into(),
                username: String::new(),
                password: String::new(),
                email: String::new(),
                starttls: false,
                active_field: FieldId::Server,
                error: None,
            },
            ConfigNeedsInput::PasswordOnly {
                server,
                port,
                username,
                starttls,
                error,
                ..
            } => Self {
                request,
                label: String::new(),
                server: server.clone(),
                port: port.to_string(),
                username: username.clone(),
                password: String::new(),
                email: String::new(),
                starttls: *starttls,
                active_field: FieldId::Password,
                error: error.clone(),
            },
        }
    }

    /// Create a setup model for editing an existing account. The caller
    /// pre-fills fields from the account config. Password is intentionally
    /// left empty (must be re-entered).
    pub fn for_edit(account_id: AccountId, fields: SetupFields) -> Self {
        Self {
            request: SetupRequest::Edit { account_id },
            label: fields.label,
            server: fields.server,
            port: fields.port,
            username: fields.username,
            password: String::new(),
            email: fields.email,
            starttls: fields.starttls,
            active_field: FieldId::Server,
            error: None,
        }
    }

    /// Title string for the setup dialog/form.
    pub fn title(&self) -> &str {
        match &self.request {
            SetupRequest::Full => "Account Setup",
            SetupRequest::PasswordOnly { .. } => "Enter Password",
            SetupRequest::Edit { .. } => "Edit Account",
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
            FieldId::Server => &self.server,
            FieldId::Port => &self.port,
            FieldId::Username => &self.username,
            FieldId::Password => &self.password,
            FieldId::Email => &self.email,
            FieldId::Starttls => unreachable!("starttls is a toggle, not text"),
        }
    }

    /// Mutable reference to a text field (None if toggle or readonly).
    fn field_mut(&mut self, field: FieldId) -> Option<&mut String> {
        if self.is_readonly(field) || field.is_toggle() {
            return None;
        }
        match field {
            FieldId::Label => Some(&mut self.label),
            FieldId::Server => Some(&mut self.server),
            FieldId::Port => Some(&mut self.port),
            FieldId::Username => Some(&mut self.username),
            FieldId::Password => Some(&mut self.password),
            FieldId::Email => Some(&mut self.email),
            FieldId::Starttls => None,
        }
    }

    /// Process an input event. Returns what the UI should do next.
    pub fn update(&mut self, input: SetupInput) -> SetupTransition {
        match input {
            SetupInput::NextField => self.cycle_field(1),
            SetupInput::PrevField => self.cycle_field(-1),
            SetupInput::Toggle => {
                if self.active_field == FieldId::Starttls && !self.is_readonly(FieldId::Starttls) {
                    self.starttls = !self.starttls;
                }
            }
            SetupInput::SetField(field, value) => {
                if !self.is_readonly(field) && !field.is_toggle() {
                    match field {
                        FieldId::Label => self.label = value,
                        FieldId::Server => self.server = value,
                        FieldId::Port => self.port = value,
                        FieldId::Username => self.username = value,
                        FieldId::Password => self.password = value,
                        FieldId::Email => self.email = value,
                        FieldId::Starttls => {}
                    }
                    self.error = None;
                }
            }
            SetupInput::SetToggle(field, value) => {
                if field == FieldId::Starttls && !self.is_readonly(field) {
                    self.starttls = value;
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
            SetupRequest::PasswordOnly {
                account_id,
                server,
                username,
                ..
            } => {
                if self.password.is_empty() {
                    self.error = Some("Password is required".into());
                    return SetupTransition::Continue;
                }

                let password_backend = store_password(username, server, &self.password);

                let mut multi = match MultiAccountFileConfig::load() {
                    Ok(Some(m)) => m,
                    _ => {
                        self.error = Some("Could not load existing config".into());
                        return SetupTransition::Continue;
                    }
                };
                match multi.accounts.iter_mut().find(|a| a.id == *account_id) {
                    Some(acct) => acct.password = password_backend,
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
                if let Some(err) = self.validate_full() {
                    self.error = Some(err);
                    return SetupTransition::Continue;
                }
                let port = self.port.trim().parse::<u16>().unwrap(); // validated above

                let server = self.server.trim().to_string();
                let username = self.username.trim().to_string();
                let email = self.email.trim().to_string();
                let label = if self.label.trim().is_empty() {
                    username.clone()
                } else {
                    self.label.trim().to_string()
                };
                let account_id = new_account_id();

                let password_backend = store_password(&username, &server, &self.password);

                let fac = FileAccountConfig {
                    id: account_id,
                    label,
                    server,
                    port,
                    username,
                    starttls: self.starttls,
                    password: password_backend,
                    email_addresses: vec![email],
                    smtp: SmtpOverrides::default(),
                };

                let mut multi = MultiAccountFileConfig::load()
                    .ok()
                    .flatten()
                    .unwrap_or(MultiAccountFileConfig {
                        accounts: Vec::new(),
                    });
                if multi
                    .accounts
                    .iter()
                    .any(|a| a.server == fac.server && a.username == fac.username)
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
                // Edit doesn't require password — empty means keep existing
                if let Some(err) = self.validate_edit() {
                    self.error = Some(err);
                    return SetupTransition::Continue;
                }
                let port = self.port.trim().parse::<u16>().unwrap();

                let server = self.server.trim().to_string();
                let username = self.username.trim().to_string();
                let email = self.email.trim().to_string();
                let label = if self.label.trim().is_empty() {
                    username.clone()
                } else {
                    self.label.trim().to_string()
                };

                let mut multi = MultiAccountFileConfig::load()
                    .ok()
                    .flatten()
                    .unwrap_or(MultiAccountFileConfig {
                        accounts: Vec::new(),
                    });

                let existing = match multi.accounts.iter().find(|a| a.id == *account_id) {
                    Some(a) => a,
                    None => {
                        self.error = Some("Account not found in config".into());
                        return SetupTransition::Continue;
                    }
                };

                // If server/username changed, keyring lookup key changes —
                // require password so we can store it under the new key
                let creds_changed =
                    existing.server != server || existing.username != username;
                if creds_changed && self.password.is_empty() {
                    self.error =
                        Some("Password required when changing server or username".into());
                    return SetupTransition::Continue;
                }

                let password_backend = if self.password.is_empty() {
                    existing.password.clone()
                } else {
                    store_password(&username, &server, &self.password)
                };

                let fac = FileAccountConfig {
                    id: account_id.clone(),
                    label,
                    server,
                    port,
                    username,
                    starttls: self.starttls,
                    password: password_backend,
                    email_addresses: vec![email],
                    smtp: SmtpOverrides::default(),
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
        }
    }

    /// Validate the current fields. Returns `None` if valid, `Some(error)` if not.
    /// Use this when the UI wants to validate before doing its own submit logic
    /// (e.g., COSMIC needs to spawn an IMAP connect task after persist).
    pub fn validate(&self) -> Option<String> {
        match &self.request {
            SetupRequest::PasswordOnly { .. } => {
                if self.password.is_empty() {
                    return Some("Password is required".into());
                }
                None
            }
            SetupRequest::Full => self.validate_full(),
            SetupRequest::Edit { .. } => self.validate_edit(),
        }
    }

    fn validate_full(&self) -> Option<String> {
        if self.server.trim().is_empty() {
            return Some("Server is required".into());
        }
        if self.username.trim().is_empty() {
            return Some("Username is required".into());
        }
        if self.password.is_empty() {
            return Some("Password is required".into());
        }
        if self.email.trim().is_empty() {
            return Some("Email address is required".into());
        }
        if self.port.trim().parse::<u16>().is_err() {
            return Some("Port must be a number (e.g. 993)".into());
        }
        None
    }

    /// Edit validation: same as full but password is optional (empty = keep existing).
    fn validate_edit(&self) -> Option<String> {
        if self.server.trim().is_empty() {
            return Some("Server is required".into());
        }
        if self.username.trim().is_empty() {
            return Some("Username is required".into());
        }
        if self.email.trim().is_empty() {
            return Some("Email address is required".into());
        }
        if self.port.trim().parse::<u16>().is_err() {
            return Some("Port must be a number (e.g. 993)".into());
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
    /// Toggle the currently active field (Starttls).
    Toggle,
    /// Set a field's entire value (for widget-based UIs like COSMIC).
    SetField(FieldId, String),
    /// Set a toggle field's value directly.
    SetToggle(FieldId, bool),
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
    pub server: String,
    pub port: String,
    pub username: String,
    pub email: String,
    pub starttls: bool,
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Try keyring, fall back to plaintext. Public so UIs with custom submit
/// logic (e.g., COSMIC with SMTP overrides) can reuse the same strategy.
pub fn store_password(username: &str, server: &str, password: &str) -> PasswordBackend {
    match keyring::set_password(username, server, password) {
        Ok(()) => {
            log::info!("Password stored in keyring for {}@{}", username, server);
            PasswordBackend::Keyring
        }
        Err(e) => {
            log::warn!("Keyring unavailable ({}), using plaintext", e);
            PasswordBackend::Plaintext {
                value: password.to_string(),
            }
        }
    }
}
