//! JMAP HTTP transport layer.
//!
//! Handles batched method calls, error classification, blob upload/download.
//! All JMAP operations go through `JmapClient`.

use std::sync::Arc;
use tokio::sync::RwLock;

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Capabilities URN constants.
pub const CAP_CORE: &str = "urn:ietf:params:jmap:core";
pub const CAP_MAIL: &str = "urn:ietf:params:jmap:mail";
pub const CAP_SUBMISSION: &str = "urn:ietf:params:jmap:submission";

/// A JMAP API request (RFC 8620 §3.3).
#[derive(Debug, Serialize)]
pub struct JmapRequest {
    pub using: Vec<String>,
    #[serde(rename = "methodCalls")]
    pub method_calls: Vec<MethodCall>,
}

/// A single method call within a JMAP request.
/// `[method_name, arguments, call_id]`
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MethodCall(pub String, pub Value, pub String);

/// A JMAP API response (RFC 8620 §3.4).
#[derive(Debug, Deserialize)]
pub struct JmapResponse {
    #[serde(rename = "methodResponses")]
    pub method_responses: Vec<MethodCall>,
    #[serde(rename = "sessionState")]
    pub session_state: Option<String>,
}

/// Errors from JMAP API calls.
#[derive(Debug, thiserror::Error)]
pub enum JmapError {
    #[error("HTTP error: {0}")]
    Http(#[from] reqwest::Error),
    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),
    #[error("JMAP method error: {method} ({error_type}): {description}")]
    MethodError {
        method: String,
        error_type: String,
        description: String,
    },
    #[error("cannotCalculateChanges — full resync required")]
    CannotCalculateChanges,
    #[error("JMAP request error: {0}")]
    RequestError(String),
    #[error("Cache error: {0}")]
    CacheError(String),
}

/// JMAP HTTP client. Holds session URLs and sends batched requests.
///
/// Auth is stored in a swappable `Arc<RwLock<String>>` and applied per-request,
/// allowing transparent token refresh for OAuth accounts.
#[derive(Debug, Clone)]
pub struct JmapClient {
    http: reqwest::Client,
    auth: Arc<RwLock<String>>,
    pub api_url: String,
    pub upload_url: String,
    pub download_url: String,
    pub event_source_url: Option<String>,
    pub account_id: String,
}

/// Whether a `reqwest::Error` represents a transient transport failure worth retrying.
fn is_transient_transport(e: &reqwest::Error) -> bool {
    e.is_connect() || e.is_timeout() || e.is_request()
}

impl JmapClient {
    pub fn new(
        api_url: String,
        upload_url: String,
        download_url: String,
        event_source_url: Option<String>,
        account_id: String,
        auth_header: String,
    ) -> Result<Self, JmapError> {
        // Validate the auth header value is usable
        reqwest::header::HeaderValue::from_str(&auth_header)
            .map_err(|e| JmapError::RequestError(format!("invalid auth header: {e}")))?;

        let mut headers = reqwest::header::HeaderMap::new();
        headers.insert(
            reqwest::header::CONTENT_TYPE,
            reqwest::header::HeaderValue::from_static("application/json"),
        );

        let http = reqwest::Client::builder()
            .default_headers(headers)
            .timeout(std::time::Duration::from_secs(60))
            .build()?;

        Ok(JmapClient {
            http,
            auth: Arc::new(RwLock::new(auth_header)),
            api_url,
            upload_url,
            download_url,
            event_source_url,
            account_id,
        })
    }

    /// Atomically swap the auth header value (e.g. after token refresh).
    pub async fn set_auth(&self, new_auth: String) {
        *self.auth.write().await = new_auth;
    }

    /// Read the current auth header value.
    pub async fn auth_header(&self) -> String {
        self.auth.read().await.clone()
    }

    /// Execute a batch of JMAP method calls in a single HTTP POST.
    ///
    /// Retries automatically on HTTP 429 (rate limit) and 503 (service unavailable)
    /// with exponential backoff (1s, 2s, 4s — max 3 retries).
    pub async fn call(&self, method_calls: Vec<MethodCall>) -> Result<JmapResponse, JmapError> {
        let request = JmapRequest {
            using: vec![
                CAP_CORE.to_string(),
                CAP_MAIL.to_string(),
                CAP_SUBMISSION.to_string(),
            ],
            method_calls,
        };

        let mut delay = std::time::Duration::from_secs(1);
        let max_retries = 3u32;

        for attempt in 0..=max_retries {
            let auth = self.auth_header().await;
            let resp = match self
                .http
                .post(&self.api_url)
                .header(reqwest::header::AUTHORIZATION, &auth)
                .json(&request)
                .send()
                .await
            {
                Ok(resp) => resp,
                Err(e) if attempt < max_retries && is_transient_transport(&e) => {
                    log::warn!(
                        "JMAP transport error — retry {}/{} in {:?}: {}",
                        attempt + 1, max_retries, delay, e
                    );
                    tokio::time::sleep(delay).await;
                    delay *= 2;
                    continue;
                }
                Err(e) => return Err(e.into()),
            };

            let status = resp.status();

            // Retry on 429 or 503
            if (status == reqwest::StatusCode::TOO_MANY_REQUESTS
                || status == reqwest::StatusCode::SERVICE_UNAVAILABLE)
                && attempt < max_retries
            {
                log::warn!("JMAP HTTP {} — retry {}/{} in {:?}", status, attempt + 1, max_retries, delay);
                tokio::time::sleep(delay).await;
                delay *= 2;
                continue;
            }

            if !status.is_success() {
                return Err(JmapError::RequestError(format!(
                    "HTTP {} from JMAP API",
                    status
                )));
            }

            let jmap_resp: JmapResponse = resp.json().await?;

            // Check for method-level errors
            for mc in &jmap_resp.method_responses {
                if mc.0 == "error" {
                    let error_type = mc.1.get("type")
                        .and_then(|v| v.as_str())
                        .unwrap_or("unknown")
                        .to_string();

                    if error_type == "cannotCalculateChanges" {
                        return Err(JmapError::CannotCalculateChanges);
                    }

                    let description = mc.1.get("description")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string();

                    return Err(JmapError::MethodError {
                        method: mc.2.clone(),
                        error_type,
                        description,
                    });
                }
            }

            return Ok(jmap_resp);
        }

        Err(JmapError::RequestError("Max retries exceeded".into()))
    }

    /// Upload a blob (RFC 8620 §6.1). Returns the blob ID.
    pub async fn upload_blob(&self, data: &[u8], content_type: &str) -> Result<String, JmapError> {
        let url = self.upload_url
            .replace("{accountId}", &self.account_id);

        let auth = self.auth_header().await;
        let resp = self
            .http
            .post(&url)
            .header(reqwest::header::AUTHORIZATION, &auth)
            .header("Content-Type", content_type)
            .body(data.to_vec())
            .send()
            .await?;

        if !resp.status().is_success() {
            return Err(JmapError::RequestError(format!(
                "Blob upload failed: HTTP {}",
                resp.status()
            )));
        }

        let body: Value = resp.json().await?;
        body.get("blobId")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
            .ok_or_else(|| JmapError::RequestError("Missing blobId in upload response".into()))
    }

    /// Download a blob (RFC 8620 §6.2). Returns raw bytes.
    pub async fn download_blob(&self, blob_id: &str) -> Result<Vec<u8>, JmapError> {
        let url = self.download_url
            .replace("{accountId}", &self.account_id)
            .replace("{blobId}", blob_id)
            .replace("{name}", "download")
            .replace("{type}", "application/octet-stream");

        let auth = self.auth_header().await;
        let resp = self.http.get(&url)
            .header(reqwest::header::AUTHORIZATION, &auth)
            .send().await?;

        if !resp.status().is_success() {
            return Err(JmapError::RequestError(format!(
                "Blob download failed: HTTP {}",
                resp.status()
            )));
        }

        Ok(resp.bytes().await?.to_vec())
    }

    /// Build a method call with the client's account ID pre-filled.
    pub fn method(&self, name: &str, mut args: Value, call_id: &str) -> MethodCall {
        if let Some(obj) = args.as_object_mut() {
            obj.insert("accountId".to_string(), Value::String(self.account_id.clone()));
        }
        MethodCall(name.to_string(), args, call_id.to_string())
    }

    /// Build a back-reference (RFC 8620 §3.7) for use in batched requests.
    pub fn result_ref(result_of: &str, name: &str, path: &str) -> Value {
        serde_json::json!({
            "resultOf": result_of,
            "name": name,
            "path": path,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn jmap_request_serializes_correctly() {
        let req = JmapRequest {
            using: vec![CAP_CORE.to_string(), CAP_MAIL.to_string()],
            method_calls: vec![
                MethodCall(
                    "Mailbox/get".to_string(),
                    serde_json::json!({"accountId": "u1234"}),
                    "0".to_string(),
                ),
            ],
        };
        let json = serde_json::to_value(&req).unwrap();
        assert_eq!(json["using"][0], "urn:ietf:params:jmap:core");
        assert_eq!(json["methodCalls"][0][0], "Mailbox/get");
        assert_eq!(json["methodCalls"][0][2], "0");
    }

    #[test]
    fn jmap_response_deserializes() {
        let json = r#"{
            "methodResponses": [
                ["Mailbox/get", {"accountId": "u1234", "list": []}, "0"]
            ],
            "sessionState": "abc123"
        }"#;
        let resp: JmapResponse = serde_json::from_str(json).unwrap();
        assert_eq!(resp.method_responses.len(), 1);
        assert_eq!(resp.method_responses[0].0, "Mailbox/get");
        assert_eq!(resp.session_state.as_deref(), Some("abc123"));
    }

    #[test]
    fn result_ref_format() {
        let r = JmapClient::result_ref("0", "Email/query", "/ids");
        assert_eq!(r["resultOf"], "0");
        assert_eq!(r["name"], "Email/query");
        assert_eq!(r["path"], "/ids");
    }
}
