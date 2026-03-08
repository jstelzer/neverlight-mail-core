//! JMAP autodiscovery via RFC 8620 §2.2.
//!
//! Probes `https://{domain}/.well-known/jmap` with basic auth to determine
//! whether a mail server supports JMAP.

use crate::config::{AccountCapabilities, Protocol};

/// Probe a mail server for JMAP support.
///
/// Tries `GET https://{domain}/.well-known/jmap` with basic auth.
/// Returns `AccountCapabilities` with protocol set to `Jmap` if the server
/// responds with a valid JMAP session object, otherwise `Imap`.
pub async fn probe_capabilities(
    domain: &str,
    username: &str,
    password: &str,
) -> AccountCapabilities {
    match try_jmap_discovery(domain, username, password).await {
        Ok(caps) => {
            log::info!(
                "JMAP discovery succeeded for {}: session_url={:?}, push={}, submission={}",
                domain,
                caps.jmap_session_url,
                caps.supports_push,
                caps.supports_submission,
            );
            caps
        }
        Err(e) => {
            log::info!("JMAP discovery failed for {}: {}", domain, e);
            AccountCapabilities::default()
        }
    }
}

async fn try_jmap_discovery(
    domain: &str,
    username: &str,
    password: &str,
) -> Result<AccountCapabilities, String> {
    let url = format!("https://{}/.well-known/jmap", domain);

    let client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::limited(3))
        .timeout(std::time::Duration::from_secs(10))
        .build()
        .map_err(|e| format!("HTTP client error: {}", e))?;

    let response = client
        .get(&url)
        .basic_auth(username, Some(password))
        .send()
        .await
        .map_err(|e| format!("GET {}: {}", url, e))?;

    if !response.status().is_success() {
        return Err(format!(
            "HTTP {} from /.well-known/jmap",
            response.status()
        ));
    }

    let body = response
        .text()
        .await
        .map_err(|e| format!("reading response body: {}", e))?;

    parse_session_object(&body, domain)
}

/// Parse a JMAP Session object (RFC 8620 §2) and extract capabilities.
fn parse_session_object(json: &str, domain: &str) -> Result<AccountCapabilities, String> {
    let session: serde_json::Value =
        serde_json::from_str(json).map_err(|e| format!("invalid JSON: {}", e))?;

    let capabilities = session
        .get("capabilities")
        .and_then(|v| v.as_object())
        .ok_or("missing or invalid 'capabilities' in session object")?;

    if !capabilities.contains_key("urn:ietf:params:jmap:mail") {
        return Err("server does not advertise urn:ietf:params:jmap:mail".into());
    }

    let supports_push = capabilities.contains_key("urn:ietf:params:jmap:websocket")
        || session.get("eventSourceUrl").and_then(|v| v.as_str()).is_some();

    let supports_submission =
        capabilities.contains_key("urn:ietf:params:jmap:submission");

    let session_url = session
        .get("apiUrl")
        .and_then(|v| v.as_str())
        .map(|api_url| {
            if api_url.starts_with("http://") || api_url.starts_with("https://") {
                api_url.to_string()
            } else {
                format!("https://{}{}", domain, api_url)
            }
        });

    Ok(AccountCapabilities {
        protocol: Protocol::Jmap,
        jmap_session_url: session_url,
        supports_push,
        supports_submission,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_fastmail_style_session() {
        let json = r#"{
            "capabilities": {
                "urn:ietf:params:jmap:core": {},
                "urn:ietf:params:jmap:mail": {},
                "urn:ietf:params:jmap:submission": {}
            },
            "apiUrl": "https://api.fastmail.com/jmap/api/",
            "eventSourceUrl": "https://api.fastmail.com/jmap/event/"
        }"#;

        let caps = parse_session_object(json, "fastmail.com").unwrap();
        assert_eq!(caps.protocol, Protocol::Jmap);
        assert_eq!(
            caps.jmap_session_url.as_deref(),
            Some("https://api.fastmail.com/jmap/api/")
        );
        assert!(caps.supports_push);
        assert!(caps.supports_submission);
    }

    #[test]
    fn parses_relative_api_url() {
        let json = r#"{
            "capabilities": {
                "urn:ietf:params:jmap:core": {},
                "urn:ietf:params:jmap:mail": {}
            },
            "apiUrl": "/jmap/"
        }"#;

        let caps = parse_session_object(json, "mail.example.com").unwrap();
        assert_eq!(
            caps.jmap_session_url.as_deref(),
            Some("https://mail.example.com/jmap/")
        );
        assert!(!caps.supports_push);
        assert!(!caps.supports_submission);
    }

    #[test]
    fn rejects_missing_mail_capability() {
        let json = r#"{
            "capabilities": {
                "urn:ietf:params:jmap:core": {}
            },
            "apiUrl": "https://api.example.com/jmap/"
        }"#;

        let err = parse_session_object(json, "example.com").unwrap_err();
        assert!(err.contains("urn:ietf:params:jmap:mail"));
    }

    #[test]
    fn rejects_invalid_json() {
        let err = parse_session_object("not json", "example.com").unwrap_err();
        assert!(err.contains("invalid JSON"));
    }

    #[test]
    fn rejects_missing_capabilities() {
        let json = r#"{"apiUrl": "https://api.example.com/jmap/"}"#;
        let err = parse_session_object(json, "example.com").unwrap_err();
        assert!(err.contains("capabilities"));
    }
}
