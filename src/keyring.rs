const SERVICE: &str = "neverlight-mail";

fn key_id(username: &str, server: &str) -> String {
    format!("{username}@{server}")
}

pub fn get_password(username: &str, server: &str) -> Result<String, String> {
    let key = key_id(username, server);
    log::debug!("keyring GET: service={SERVICE:?} key={key:?}");
    let entry = keyring::Entry::new(SERVICE, &key).map_err(|e| {
        log::error!("keyring Entry::new failed for key={key:?}: {e}");
        format!("keyring error: {e}")
    })?;
    entry.get_password().map_err(|e| {
        log::warn!("keyring get_password failed for key={key:?}: {e}");
        format!("keyring get: {e}")
    })
}

pub fn set_password(username: &str, server: &str, password: &str) -> Result<(), String> {
    let key = key_id(username, server);
    log::debug!("keyring SET: service={SERVICE:?} key={key:?}");
    let entry = keyring::Entry::new(SERVICE, &key).map_err(|e| {
        log::error!("keyring Entry::new failed for key={key:?}: {e}");
        format!("keyring error: {e}")
    })?;
    entry.set_password(password).map_err(|e| {
        log::error!("keyring set_password failed for key={key:?}: {e}");
        format!("keyring set: {e}")
    })
}

pub fn delete_password(username: &str, server: &str) -> Result<(), String> {
    let key = key_id(username, server);
    log::debug!("keyring DELETE: service={SERVICE:?} key={key:?}");
    let entry = keyring::Entry::new(SERVICE, &key).map_err(|e| format!("keyring error: {e}"))?;
    entry.delete_credential().map_err(|e| {
        log::warn!("keyring delete failed for key={key:?}: {e}");
        format!("keyring delete: {e}")
    })
}

// ---------------------------------------------------------------------------
// OAuth refresh token storage
// ---------------------------------------------------------------------------

fn oauth_refresh_key(account_id: &str) -> String {
    format!("oauth-refresh:{account_id}")
}

pub fn get_oauth_refresh(account_id: &str) -> Result<String, String> {
    let key = oauth_refresh_key(account_id);
    log::debug!("keyring GET oauth: service={SERVICE:?} key={key:?}");
    let entry = keyring::Entry::new(SERVICE, &key).map_err(|e| format!("keyring error: {e}"))?;
    entry.get_password().map_err(|e| {
        log::warn!("keyring get oauth refresh failed for key={key:?}: {e}");
        format!("keyring get: {e}")
    })
}

pub fn set_oauth_refresh(account_id: &str, refresh_token: &str) -> Result<(), String> {
    let key = oauth_refresh_key(account_id);
    log::debug!("keyring SET oauth: service={SERVICE:?} key={key:?}");
    let entry = keyring::Entry::new(SERVICE, &key).map_err(|e| format!("keyring error: {e}"))?;
    entry.set_password(refresh_token).map_err(|e| {
        log::error!("keyring set oauth refresh failed for key={key:?}: {e}");
        format!("keyring set: {e}")
    })
}

pub fn delete_oauth_refresh(account_id: &str) -> Result<(), String> {
    let key = oauth_refresh_key(account_id);
    log::debug!("keyring DELETE oauth: service={SERVICE:?} key={key:?}");
    let entry = keyring::Entry::new(SERVICE, &key).map_err(|e| format!("keyring error: {e}"))?;
    entry.delete_credential().map_err(|e| {
        log::warn!("keyring delete oauth refresh failed for key={key:?}: {e}");
        format!("keyring delete: {e}")
    })
}
