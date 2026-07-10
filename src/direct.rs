//! Headless WebSocket account creation and streaming.

use anyhow::{anyhow, Result};
use base64::{engine::general_purpose, Engine as _};
use futures::{SinkExt, StreamExt};
use reqwest::cookie::{CookieStore, Jar};
use reqwest::Client;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;
use tokio::net::TcpStream;
use tokio::time::timeout;
use tokio_socks::tcp::Socks5Stream;
use tokio_tungstenite::{client_async_tls_with_config, MaybeTlsStream, WebSocketStream};
use tungstenite::{client::IntoClientRequest, protocol::WebSocketConfig, Message};

use rand::Rng;
use crate::account_pool::Account;
use crate::config::Config;
use crate::filter::InjectionFilter;
use crate::models::resolve_model;
use crate::utils::{gen_email, now_secs};
use futures::stream::BoxStream;
use tracing::{debug, error, info, warn};

use once_cell::sync::Lazy;
use std::sync::Mutex;
use std::time::Instant;

/// Tracks the next-allowed send time per use.ai proxy (Tor exit). Requests
/// through the same exit are spaced by `use_ai_per_proxy_cooldown_ms` so the
/// per-IP rate limit doesn't trip. Keyed by proxy URL, or "direct" for no
/// proxy. use.ai rate-limits per IP, not per account, so this is per-proxy.
static PER_PROXY_NEXT_SEND: Lazy<Mutex<HashMap<String, Instant>>> =
    Lazy::new(|| Mutex::new(HashMap::new()));

/// Gate consecutive use.ai sends through the same proxy. Reserves the next
/// available slot under the lock so concurrent requests space out rather than
/// all sleeping and waking together, then sleeps for the remaining wait.
async fn await_per_proxy_cooldown(proxy_key: &str, cooldown: Duration) {
    if cooldown.is_zero() {
        return;
    }
    let wait = {
        let mut map = PER_PROXY_NEXT_SEND.lock().unwrap();
        let now = Instant::now();
        let earliest = map.get(proxy_key).copied().unwrap_or(now);
        let wait = earliest.saturating_duration_since(now);
        map.insert(proxy_key.to_string(), earliest + cooldown);
        wait
    };
    if !wait.is_zero() {
        debug!(
            "per-proxy cooldown: waiting {:?} before sending via {}",
            wait, proxy_key
        );
        tokio::time::sleep(wait).await;
    }
}

const USER_AGENT: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/144.0.0.0 Safari/537.36";

// ---------- File upload helper ----------

/// Upload a file (base64 encoded) to files.use.ai and return the public URL.
async fn upload_file_to_files(
    base64_data: &str,
    filename: &str,
    proxy_url: Option<&str>,
) -> Result<String> {
    let raw_data = general_purpose::STANDARD
        .decode(base64_data)
        .map_err(|e| anyhow!("Invalid base64: {}", e))?;
    let media_type = guess_media_type(filename, Some(&raw_data));

    upload_bytes_to_files(raw_data, filename, &media_type, proxy_url).await
}

async fn upload_bytes_to_files(
    raw_data: Vec<u8>,
    filename: &str,
    media_type: &str,
    proxy_url: Option<&str>,
) -> Result<String> {
    let media_type = if media_type.is_empty() {
        guess_media_type(filename, Some(&raw_data))
    } else {
        media_type.to_string()
    };

    let client_builder = Client::builder()
        .timeout(Duration::from_secs(30))
        .user_agent(USER_AGENT)
        .no_proxy();

    let client = if let Some(url) = proxy_url {
        client_builder.proxy(reqwest::Proxy::all(url)?).build()?
    } else {
        client_builder.build()?
    };

    let part = reqwest::multipart::Part::bytes(raw_data)
        .file_name(filename.to_string())
        .mime_str(&media_type)?;
    let form = reqwest::multipart::Form::new()
        .part("file", part)
        .text("name", filename.to_string())
        .text("type", media_type);

    let resp = client
        .post("https://files.use.ai/upload")
        .multipart(form)
        .header("Origin", "https://use.ai")
        .header("Referer", "https://use.ai/")
        .send()
        .await
        .map_err(|e| anyhow!("Upload request failed: {}", e))?;

    let status = resp.status();
    if !status.is_success() {
        let text = resp.text().await.unwrap_or_default();
        anyhow::bail!("Upload failed: {} - {}", status, text);
    }

    let data: Value = resp.json().await?;
    let file_url = data["url"]
        .as_str()
        .ok_or_else(|| anyhow!("Upload response missing 'url'"))?;
    let full_url = if file_url.starts_with('/') {
        format!("https://files.use.ai{}", file_url)
    } else {
        file_url.to_string()
    };

    info!("Uploaded file: {}", full_url);
    Ok(full_url)
}

async fn upload_remote_image_to_files(
    image_url: &str,
    filename: &str,
    proxy_url: Option<&str>,
) -> Result<(String, String)> {
    let client_builder = Client::builder()
        .timeout(Duration::from_secs(30))
        .user_agent(USER_AGENT)
        .no_proxy();

    let client = if let Some(url) = proxy_url {
        client_builder.proxy(reqwest::Proxy::all(url)?).build()?
    } else {
        client_builder.build()?
    };

    let resp = client
        .get(image_url)
        .header("Accept", "image/*,*/*;q=0.8")
        .send()
        .await
        .map_err(|e| anyhow!("Image download failed: {}", e))?;

    let status = resp.status();
    if !status.is_success() {
        let text = resp.text().await.unwrap_or_default();
        anyhow::bail!("Image download failed: {} - {}", status, text);
    }

    let content_type = resp
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.split(';').next())
        .filter(|value| value.starts_with("image/"))
        .map(ToOwned::to_owned);
    let bytes = resp.bytes().await?.to_vec();
    let media_type = content_type.unwrap_or_else(|| guess_media_type(filename, Some(&bytes)));
    let uploaded_url = upload_bytes_to_files(bytes, filename, &media_type, proxy_url).await?;

    Ok((uploaded_url, media_type))
}

fn is_image_media_type(media_type: &str) -> bool {
    media_type.starts_with("image/")
}

fn guess_media_type(filename: &str, content: Option<&[u8]>) -> String {
    let ext = filename.rsplit('.').next().unwrap_or("").to_lowercase();
    match ext.as_str() {
        "png" => "image/png".to_string(),
        "jpg" | "jpeg" => "image/jpeg".to_string(),
        "gif" => "image/gif".to_string(),
        "webp" => "image/webp".to_string(),
        "avif" => "image/avif".to_string(),
        "pdf" => "application/pdf".to_string(),
        "docx" => {
            "application/vnd.openxmlformats-officedocument.wordprocessingml.document".to_string()
        }
        "doc" => "application/msword".to_string(),
        "txt" => "text/plain".to_string(),
        "csv" => "text/csv".to_string(),
        _ => {
            if let Some(bytes) = content {
                if bytes.starts_with(&[0x89, 0x50, 0x4E, 0x47]) {
                    return "image/png".to_string();
                }
                if bytes.starts_with(&[0xFF, 0xD8, 0xFF]) {
                    return "image/jpeg".to_string();
                }
                if bytes.starts_with(&[0x47, 0x49, 0x46, 0x38]) {
                    return "image/gif".to_string();
                }
                if bytes.starts_with(&[0x52, 0x49, 0x46, 0x46]) {
                    return "image/webp".to_string();
                }
                if bytes.starts_with(&[0x25, 0x50, 0x44, 0x46]) {
                    return "application/pdf".to_string();
                }
            }
            "application/octet-stream".to_string()
        }
    }
}

// ---------- Account creation ----------

pub async fn create_account(proxy_url: Option<&str>) -> Result<Account> {
    let attempts = vec![proxy_url];
    let mut last_err = None;

    for (idx, proxy) in attempts.into_iter().enumerate() {
        let proxy_desc = proxy.unwrap_or("direct");
        debug!("Account creation attempt {} using {}", idx + 1, proxy_desc);

        let mut retry_count = 0;
        while retry_count < 3 {
            match create_account_once(proxy).await {
                Ok(account) => return Ok(account),
                Err(e) => {
                    if is_rate_limit_error(&e) {
                        let wait = Duration::from_secs(2u64.pow(retry_count + 1));
                        warn!(
                            "Received 429 using {}, waiting {:?} before retry",
                            proxy_desc, wait
                        );
                        last_err = Some(e);
                        retry_count += 1;
                        tokio::time::sleep(wait).await;
                        continue;
                    }

                    error!(
                        "Account creation attempt {} using {} failed: {:?}",
                        idx + 1,
                        proxy_desc,
                        e
                    );
                    last_err = Some(e);
                    break;
                }
            }
        }

        if retry_count >= 3 {
            error!("Retry limit reached for {}", proxy_desc);
        }
    }

    Err(last_err.unwrap_or_else(|| anyhow!("all account creation attempts failed")))
}

fn is_rate_limit_error(err: &anyhow::Error) -> bool {
    err.to_string().contains("429") || format!("{:?}", err).contains("429")
}

/// Sentinel for a use.ai `rate-limit-error` frame. Carried as a typed source
/// inside the `anyhow::Error` yielded by the stream so consumers can detect it
/// via `is_stream_rate_limit_error` and avoid injecting the raw error text
/// into the model's content stream.
#[derive(Debug)]
pub struct RateLimitError;

impl std::fmt::Display for RateLimitError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "rate limit error")
    }
}

impl std::error::Error for RateLimitError {}

/// True if a stream error is a use.ai rate-limit error. These should not be
/// surfaced into the client-visible content stream; the request either gets
/// retried transparently (pre-content) or ends cleanly (post-content).
pub fn is_stream_rate_limit_error(err: &anyhow::Error) -> bool {
    err.downcast_ref::<RateLimitError>().is_some()
}

/// Peeks the first item of a use.ai completion stream before the caller
/// commits to an SSE 200 response. A stream that opens with a fatal
/// rate-limit error (no content emitted, fail-fast because the same bound
/// Tor exit can't be retried) is returned as `Err` so the caller can map it
/// to a real HTTP 429 and let the client retry with a fresh account/exit —
/// instead of yielding an empty `finish_reason: "stop"` completion that
/// clients report as "model returned no content". Any other first item (Ok
/// content, a non-rate-limit error, or an empty stream) is prepended back
/// onto the stream and returned for normal consumption, preserving existing
/// behavior.
pub async fn peek_stream_start(
    mut stream: BoxStream<'static, Result<String>>,
) -> Result<BoxStream<'static, Result<String>>> {
    let first = stream.next().await;
    match first {
        Some(Err(e)) if is_stream_rate_limit_error(&e) => Err(e),
        Some(item) => {
            let prepend = futures::stream::once(futures::future::ready(item));
            Ok(Box::pin(prepend.chain(stream))
                as BoxStream<'static, Result<String>>)
        }
        None => Ok(Box::pin(stream) as BoxStream<'static, Result<String>>),
    }
}

fn build_client_with_jar(proxy_url: Option<&str>) -> Result<(Client, Arc<Jar>)> {
    let jar = Arc::new(Jar::default());
    let mut default_headers = reqwest::header::HeaderMap::new();
    default_headers.insert(
        "Origin",
        tungstenite::http::HeaderValue::from_static("https://use.ai"),
    );
    default_headers.insert(
        "Referer",
        tungstenite::http::HeaderValue::from_static("https://use.ai/"),
    );
    // Full Chrome header set so use.ai's signup anti-bot check sees a browser
    // rather than a bare reqwest client. Matches the USER_AGENT (Chrome 144).
    default_headers.insert("Accept", tungstenite::http::HeaderValue::from_static("*/*"));
    default_headers.insert(
        "Accept-Language",
        tungstenite::http::HeaderValue::from_static("en-US,en;q=0.9"),
    );
    default_headers.insert(
        "sec-ch-ua",
        tungstenite::http::HeaderValue::from_static(
            "\"Google Chrome\";v=\"144\", \"Chromium\";v=\"144\", \"Not.A/Brand\";v=\"99\"",
        ),
    );
    default_headers.insert(
        "sec-ch-ua-mobile",
        tungstenite::http::HeaderValue::from_static("?0"),
    );
    default_headers.insert(
        "sec-ch-ua-platform",
        tungstenite::http::HeaderValue::from_static("\"Windows\""),
    );
    default_headers.insert(
        "sec-fetch-site",
        tungstenite::http::HeaderValue::from_static("same-origin"),
    );
    default_headers.insert(
        "sec-fetch-mode",
        tungstenite::http::HeaderValue::from_static("cors"),
    );
    default_headers.insert(
        "sec-fetch-dest",
        tungstenite::http::HeaderValue::from_static("empty"),
    );
    let client_builder = Client::builder()
        .timeout(Duration::from_secs(30))
        .cookie_provider(jar.clone())
        .user_agent(USER_AGENT)
        .default_headers(default_headers)
        .no_proxy();

    let client = if let Some(url) = proxy_url {
        client_builder.proxy(reqwest::Proxy::all(url)?).build()?
    } else {
        client_builder.build()?
    };

    Ok((client, jar))
}

async fn create_account_once(proxy_url: Option<&str>) -> Result<Account> {
    let (client, jar) = build_client_with_jar(proxy_url)?;
    let email = gen_email();
    let cfg = Config::load().unwrap_or_default();
    let auth_base = cfg.direct.auth_base;

    // 0. Seed the cookie jar with a GET to the use.ai root. A real browser
    // loads the page before submitting signup, and use.ai may require a
    // cookie/CSRF token set by this initial load before the email-login POST
    // is accepted (otherwise it returns 403 "Access denied"). We don't care
    // about the body — only the cookies the response sets on the jar.
    if let Err(e) = client.get("https://use.ai/").send().await {
        debug!("seed GET to use.ai failed (continuing): {}", e);
    }

    // 1. email-login
    let resp = client
        .post(format!("{}/email-login", auth_base))
        .json(&json!({ "email": email }))
        .send()
        .await
        .map_err(|e| {
            error!("email-login request failed: {:?}", e);
            anyhow!("email-login request failed: {}", e)
        })?;
    if !resp.status().is_success() {
        let status = resp.status();
        // Log key response headers so we can tell whether the 403 is a
        // Cloudflare block (server: cloudflare, cf-ray, cf-mitigated) or
        // use.ai's own backend rejecting the signup (no cf-* headers).
        let server = resp
            .headers()
            .get("server")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("?")
            .to_string();
        let cf_ray = resp
            .headers()
            .get("cf-ray")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("-")
            .to_string();
        let cf_mitigated = resp
            .headers()
            .get("cf-mitigated")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("-")
            .to_string();
        let text = resp.text().await.unwrap_or_default();
        error!(
            "email-login failed: {} - {} | server={} cf-ray={} cf-mitigated={}",
            status, text, server, cf_ray, cf_mitigated
        );
        anyhow::bail!("email-login failed: {} - {}", status, text);
    }

    // 2. sign-in/credentials
    let resp = client
        .post(format!("{}/sign-in/credentials", auth_base))
        .json(&json!({
            "email": email,
            "mixpanelUserId": uuid::Uuid::new_v4().to_string(),
            "guestId": uuid::Uuid::new_v4().to_string(),
            "mid": uuid::Uuid::new_v4().to_string(),
        }))
        .send()
        .await
        .map_err(|e| {
            error!("sign-in/credentials request failed: {:?}", e);
            anyhow!("sign-in/credentials request failed: {}", e)
        })?;
    if !resp.status().is_success() {
        let status = resp.status();
        let text = resp.text().await.unwrap_or_default();
        error!("sign-in/credentials failed: {} - {}", status, text);
        anyhow::bail!("sign-in/credentials failed: {} - {}", status, text);
    }
    // Capture the short-lived JWT from the set-auth-jwt header. This is the
    // worker auth token the browser sends as ?token=<JWT> on the WS URL.
    let set_auth_jwt = resp
        .headers()
        .get("set-auth-jwt")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();
    if !set_auth_jwt.is_empty() {
        debug!(
            "sign-in/credentials returned set-auth-jwt: {}...",
            &set_auth_jwt[..set_auth_jwt.len().min(16)]
        );
    }
    // Consume the body so the connection can be reused for get-session.
    let _ = resp.text().await;

    // 3. get-session
    let resp = client
        .get(format!("{}/get-session", auth_base))
        .send()
        .await
        .map_err(|e| {
            error!("get-session request failed: {:?}", e);
            anyhow!("get-session request failed: {}", e)
        })?;
    if !resp.status().is_success() {
        let status = resp.status();
        let text = resp.text().await.unwrap_or_default();
        error!("get-session failed: {} - {}", status, text);
        anyhow::bail!("get-session failed: {} - {}", status, text);
    }
    let body: Value = resp.json().await?;
    let user_id = body["user"]["id"]
        .as_str()
        .ok_or_else(|| anyhow!("user id not found"))?
        .to_string();

    // The WS agent endpoint (agents.use.ai) authenticates via the session
    // token. We capture both the JWT accessToken and the opaque session.token
    // for experimentation. Currently trying the opaque token as Bearer since
    // the JWT accessToken (alone, with cookie, with origin) all fail AUTH_REQUIRED.
    let access_token_jwt = body["session"]["accessToken"]
        .as_str()
        .ok_or_else(|| anyhow!("session accessToken not found in get-session response"))?
        .to_string();

    let opaque_token = body["session"]["token"].as_str().unwrap_or("").to_string();

    // The agent gateway now requires an app_token query param on the WS URL.
    // It's issued alongside session_data by get-session.
    let app_token = body["session"]["appToken"]
        .as_str()
        .or_else(|| body["appToken"].as_str())
        .map(|s| s.to_string());
    if let Some(at) = &app_token {
        debug!("session appToken: {}...", &at[..at.len().min(16)]);
    } else {
        debug!("session appToken not present in get-session response");
    }

    // Decode JWT header+payload for debugging (no signature verification).
    debug!(
        "accessToken JWT (first 80 chars): {}...",
        &access_token_jwt[..access_token_jwt.len().min(80)]
    );
    if !opaque_token.is_empty() {
        debug!(
            "session.token (opaque): {}...",
            &opaque_token[..opaque_token.len().min(16)]
        );
    }
    if !set_auth_jwt.is_empty() {
        debug!(
            "set-auth-jwt header: {}...",
            &set_auth_jwt[..set_auth_jwt.len().min(16)]
        );
    }

    // Prefer the set-auth-jwt header (short-lived worker JWT for the WS
    // ?token= query param). Fall back to opaque session.token, then JWT
    // accessToken.
    let token = if !set_auth_jwt.is_empty() {
        set_auth_jwt
    } else if !opaque_token.is_empty() {
        opaque_token
    } else {
        access_token_jwt
    };

    let url = "https://api.use.ai".parse()?;
    let cookie_header = jar
        .cookies(&url)
        .and_then(|value| value.to_str().ok().map(ToOwned::to_owned))
        .unwrap_or_default();

    info!(
        "Account created: {} (user: {})",
        email,
        user_id.chars().take(8).collect::<String>()
    );
    Ok(Account {
        email,
        user_id,
        cookie_header,
        token,
        app_token,
        proxy_url: proxy_url.map(String::from),
        born: now_secs(),
    })
}

/// Refresh the session cookies and extract a fresh worker auth JWT before
/// connecting to the agent WebSocket.
///
/// The __Secure-better-auth.session_data cookie has a 60-second TTL, so
/// pooled accounts must be refreshed before each WS connection or the agent
/// gateway rejects them with AUTH_REQUIRED (4001).
///
/// The get-session response includes a set-auth-jwt header containing the
/// short-lived worker JWT. This is the same JWT the browser gets from
/// authClient.token() and sends as ?token=<JWT> in the WS URL.
/// Refresh the session cookies and extract a fresh worker auth JWT and app attestation token.
async fn refresh_session(
    account: &Account,
    proxy_url: Option<&str>,
) -> Result<(String, String, Option<String>)> {
    let cfg = Config::load().unwrap_or_default();
    let auth_base = cfg.direct.auth_base;
    let debug_protocol = cfg.logging.debug_protocol;

    let (client, jar) = build_client_with_jar(proxy_url)?;
    let url = "https://api.use.ai".parse()?;

    // Seed the jar with the long-lived session_token cookie.
    for cookie_pair in account.cookie_header.split("; ") {
        if cookie_pair.starts_with("__Secure-better-auth.session_token=") {
            jar.add_cookie_str(cookie_pair, &url);
        }
    }

    // 1. Refresh session_data cookie.
    let resp = client
        .get(format!("{}/get-session", auth_base))
        .send()
        .await
        .map_err(|e| anyhow!("refresh get-session failed: {}", e))?;

    if !resp.status().is_success() {
        let status = resp.status();
        let text = resp.text().await.unwrap_or_default();
        anyhow::bail!("refresh get-session failed: {} - {}", status, text);
    }

    // Extract the short-lived JWT (token) from the set-auth-jwt header.
    let token = resp
        .headers()
        .get("set-auth-jwt")
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_string())
        .unwrap_or_else(|| account.token.clone());

    // Consume the body so the connection can be reused.
    let body: Value = resp.json().await?;

    // The session data might contain the user ID – use it if available,
    // otherwise fall back to the account's stored user_id.
    let user_id = body["user"]["id"]
        .as_str()
        .unwrap_or(&account.user_id)
        .to_string();

    // 2. Fetch the app attestation token.
    let app_token = match fetch_app_attestation(&client, &auth_base, &user_id).await {
        Ok(at) => Some(at),
        Err(e) => {
            warn!("Failed to fetch app attestation token: {}", e);
            account.app_token.clone() // fallback to stored (if any)
        }
    };

    // The jar now has the fresh session_data.
    let cookie_header = jar
        .cookies(&url)
        .and_then(|value| value.to_str().ok().map(ToOwned::to_owned))
        .unwrap_or_else(|| account.cookie_header.clone());

    if debug_protocol {
        debug!(
            "Refreshed session for user {} (token: {}..., app_token: {})",
            account.user_id.chars().take(8).collect::<String>(),
            if token.is_empty() {
                "(none)"
            } else {
                &token[..token.len().min(16)]
            },
            app_token
                .as_deref()
                .map(|s| &s[..s.len().min(16)])
                .unwrap_or("(none)")
        );
    }

    Ok((cookie_header, token, app_token))
}

/// Fetch the app attestation token from the `/v1/auth/app-attestation` endpoint.
async fn fetch_app_attestation(client: &Client, auth_base: &str, user_id: &str) -> Result<String> {
    let response = client
        .post(format!("{}/app-attestation", auth_base))
        .header("Origin", "https://use.ai")
        .header("Referer", "https://use.ai/")
        .json(&serde_json::json!({ "userId": user_id }))
        .send()
        .await
        .map_err(|e| anyhow!("app-attestation request failed: {}", e))?;

    if !response.status().is_success() {
        let status = response.status();
        let text = response.text().await.unwrap_or_default();
        anyhow::bail!("app-attestation request failed: {} - {}", status, text);
    }

    let body: Value = response.json().await?;
    let app_token = body["token"]
        .as_str()
        .ok_or_else(|| anyhow!("app-attestation response missing 'token'"))?
        .to_string();

    Ok(app_token)
}

// ---------- WebSocket connection with SOCKS5 ----------

async fn connect_websocket_with_proxy(
    uri: &str,
    proxy_url: Option<&str>,
    open_timeout: Duration,
    _bearer_token: Option<&str>,
    cookie: Option<&str>,
    debug_protocol: bool,
) -> Result<WebSocketStream<MaybeTlsStream<TcpStream>>> {
    let mut request = uri.into_client_request()?;
    request.headers_mut().insert(
        "User-Agent",
        tungstenite::http::HeaderValue::from_static(USER_AGENT),
    );

    // Origin header is required by use.ai's agent gateway — the browser always
    // sends it and the backend may reject connections without it.
    request.headers_mut().insert(
        "Origin",
        tungstenite::http::HeaderValue::from_static("https://use.ai"),
    );

    // NOTE: No Authorization header. The Python reference (verified 2026-06-26)
    // sent only Cookie + Origin with no Bearer, and the browser does the same.
    // Bearer has been tried (JWT accessToken, opaque session.token) and does
    // not prevent AUTH_REQUIRED. The agent gateway authenticates via the
    // session cookie alone.

    // Send the session cookie too.
    if let Some(cookie) = cookie.filter(|c| !c.is_empty()) {
        request
            .headers_mut()
            .insert("Cookie", tungstenite::http::HeaderValue::from_str(cookie)?);
    }

    // Log the outgoing request headers for auth debugging.
    if debug_protocol {
        debug!("WS upgrade to {} | headers: {:?}", uri, request.headers());
    }

    if let Some(proxy) = proxy_url {
        let (host, port) = parse_socks5_proxy(proxy)?;
        let proxy_endpoint = format!("{}:{}", host, port);
        let target_uri = tungstenite::http::Uri::try_from(uri)?;
        let target_host = target_uri.host().unwrap_or("agents.use.ai");
        let target_port = target_uri.port_u16().unwrap_or(443);

        let sock_stream = timeout(
            open_timeout,
            Socks5Stream::connect((host, port), (target_host, target_port)),
        )
        .await
        .map_err(|_| anyhow!("SOCKS connect timeout via {}", proxy_endpoint))?
        .map_err(|e| anyhow!("SOCKS connection failed via {}: {}", proxy_endpoint, e))?;

        let config = WebSocketConfig::default();
        match client_async_tls_with_config(request, sock_stream.into_inner(), Some(config), None)
            .await
        {
            Ok((ws, response)) => {
                if debug_protocol {
                    debug!(
                        "WS upgrade response via {}: {} (host: {})",
                        proxy_endpoint,
                        response.status(),
                        proxy_url.unwrap_or("?")
                    );
                }
                Ok(ws)
            }
            Err(e) => {
                // tungstenite wraps the HTTP rejection inside WebSocketError::Http(...).
                if let tungstenite::Error::Http(http_response) = &e {
                    let status = http_response.status();
                    let body_preview = match http_response.body().as_ref() {
                        Some(bytes) => String::from_utf8_lossy(bytes).to_string(),
                        None => String::new(),
                    };
                    warn!(
                        "WS upgrade rejected via {}: status={} body={:?}",
                        proxy_endpoint, status, body_preview
                    );
                    // Surface the status code to the caller so it can react
                    // (rotate proxy/account) instead of opaque retry failures.
                    return Err(anyhow!(
                        "WS upgrade rejected by agent gateway: {} - {}",
                        status,
                        body_preview
                    ));
                }
                warn!("WS upgrade error via {}: {}", proxy_endpoint, e);
                Err(anyhow!("WS upgrade failed via {}: {}", proxy_endpoint, e))
            }
        }
    } else {
        let config = WebSocketConfig::default();
        match timeout(
            open_timeout,
            tokio_tungstenite::connect_async_with_config(request, Some(config), true),
        )
        .await
        {
            Ok(Ok((ws, response))) => {
                if debug_protocol {
                    debug!("WS upgrade response (direct): {}", response.status());
                }
                Ok(ws)
            }
            Ok(Err(e)) => {
                if let tungstenite::Error::Http(http_response) = &e {
                    let status = http_response.status();
                    let body_preview = match http_response.body().as_ref() {
                        Some(bytes) => String::from_utf8_lossy(bytes).to_string(),
                        None => String::new(),
                    };
                    warn!(
                        "WS upgrade rejected (direct): status={} body={:?}",
                        status, body_preview
                    );
                    return Err(anyhow!(
                        "WS upgrade rejected by agent gateway: {} - {}",
                        status,
                        body_preview
                    ));
                }
                warn!("WS upgrade error (direct): {}", e);
                Err(anyhow!("WS upgrade failed (direct): {}", e))
            }
            Err(_) => {
                warn!("WS upgrade timed out (direct)");
                Err(anyhow!("WebSocket open timeout"))
            }
        }
    }
}

fn parse_socks5_proxy(proxy: &str) -> Result<(&str, u16)> {
    let Some(rest) = proxy
        .strip_prefix("socks5h://")
        .or_else(|| proxy.strip_prefix("socks5://"))
    else {
        anyhow::bail!("only socks5 proxies supported for WebSocket");
    };

    let host_port = rest.trim_end_matches('/');
    if host_port.is_empty()
        || host_port.contains('@')
        || host_port.contains('/')
        || host_port.contains('?')
        || host_port.contains('#')
    {
        anyhow::bail!("SOCKS proxy URL must be socks5://host:port without credentials, path, query, or fragment");
    }

    let (host, port) = host_port
        .rsplit_once(':')
        .ok_or_else(|| anyhow!("proxy requires port"))?;
    if host.is_empty() {
        anyhow::bail!("proxy requires host");
    }

    Ok((host, port.parse::<u16>()?))
}

// ---------- Structured content parsing ----------

fn data_uri_parts(value: &str) -> Option<(&str, &str)> {
    let (header, data) = value.split_once(',')?;
    if header.starts_with("data:") {
        Some((header, data))
    } else {
        None
    }
}

fn media_type_from_data_uri_header(header: &str, fallback: &str) -> String {
    header
        .strip_prefix("data:")
        .and_then(|rest| rest.split(';').next())
        .filter(|media_type| !media_type.is_empty())
        .unwrap_or(fallback)
        .to_string()
}

async fn file_part_from_data_or_url(
    data_or_url: &str,
    filename: &str,
    explicit_media_type: Option<&str>,
    proxy_url: Option<&str>,
) -> Result<Value> {
    let guessed_media_type = explicit_media_type
        .map(ToOwned::to_owned)
        .unwrap_or_else(|| guess_media_type(filename, None));

    let (url, media_type) = if let Some((header, base64_data)) = data_uri_parts(data_or_url) {
        let media_type = media_type_from_data_uri_header(header, &guessed_media_type);
        let url = upload_file_to_files(base64_data, filename, proxy_url).await?;
        (url, media_type)
    } else if data_or_url.starts_with("http://") || data_or_url.starts_with("https://") {
        if is_image_media_type(&guessed_media_type) {
            upload_remote_image_to_files(data_or_url, filename, proxy_url).await?
        } else {
            (data_or_url.to_string(), guessed_media_type)
        }
    } else {
        let url = upload_file_to_files(data_or_url, filename, proxy_url).await?;
        (url, guessed_media_type)
    };
    Ok(json!({
        "type": "file",
        "mediaType": media_type,
        "url": url,
        "filename": filename,
    }))
}

async fn build_parts_from_content(content: &Value, proxy_url: Option<&str>) -> Result<Vec<Value>> {
    let mut parts = Vec::new();
    let mut recognized_content = false;

    match content {
        Value::String(text) => {
            recognized_content = true;
            let text = sanitize_inbound_text(text);
            if !text.is_empty() {
                parts.push(json!({ "type": "text", "text": text }));
            }
        }
        Value::Object(obj) => {
            if let Some(text) = obj.get("text").and_then(|v| v.as_str()) {
                recognized_content = true;
                let text = sanitize_inbound_text(text);
                if !text.is_empty() {
                    parts.push(json!({ "type": "text", "text": text }));
                }
            }

            if let Some(image_data) = obj.get("image").and_then(|v| v.as_str()) {
                recognized_content = true;
                let filename = obj
                    .get("filename")
                    .and_then(|v| v.as_str())
                    .unwrap_or("image.png");
                let media_type = obj.get("media_type").and_then(|v| v.as_str());
                parts.push(
                    file_part_from_data_or_url(image_data, filename, media_type, proxy_url).await?,
                );
            }

            if let Some(file_url) = obj.get("file_url").and_then(|v| v.as_str()) {
                recognized_content = true;
                let filename = obj
                    .get("filename")
                    .and_then(|v| v.as_str())
                    .unwrap_or("file");
                let media_type = obj.get("media_type").and_then(|v| v.as_str());
                parts.push(
                    file_part_from_data_or_url(file_url, filename, media_type, proxy_url).await?,
                );
            }
        }
        Value::Array(arr) => {
            for item in arr {
                let Some(obj) = item.as_object() else {
                    warn!("Skipping non-object item in content array");
                    continue;
                };

                match obj.get("type").and_then(|v| v.as_str()) {
                    Some("text") => {
                        recognized_content = true;
                        if let Some(text) = obj.get("text").and_then(|v| v.as_str()) {
                            let text = sanitize_inbound_text(text);
                            if !text.is_empty() {
                                parts.push(json!({ "type": "text", "text": text }));
                            }
                        }
                    }
                    Some("image_url") => {
                        recognized_content = true;
                        if let Some(url) = obj
                            .get("image_url")
                            .and_then(|v| v.get("url"))
                            .and_then(|v| v.as_str())
                        {
                            let filename = obj
                                .get("filename")
                                .and_then(|v| v.as_str())
                                .unwrap_or("image.png");
                            parts.push(
                                file_part_from_data_or_url(url, filename, None, proxy_url).await?,
                            );
                        }
                    }
                    Some("file") => {
                        recognized_content = true;
                        if let Some(file_obj) = obj.get("file").and_then(|v| v.as_object()) {
                            let filename = file_obj
                                .get("filename")
                                .and_then(|v| v.as_str())
                                .or_else(|| obj.get("filename").and_then(|v| v.as_str()))
                                .unwrap_or("file");
                            let media_type = file_obj
                                .get("media_type")
                                .and_then(|v| v.as_str())
                                .or_else(|| obj.get("media_type").and_then(|v| v.as_str()));

                            if let Some(url) = file_obj.get("url").and_then(|v| v.as_str()) {
                                parts.push(
                                    file_part_from_data_or_url(
                                        url, filename, media_type, proxy_url,
                                    )
                                    .await?,
                                );
                            } else if let Some(data) = file_obj.get("data").and_then(|v| v.as_str())
                            {
                                parts.push(
                                    file_part_from_data_or_url(
                                        data, filename, media_type, proxy_url,
                                    )
                                    .await?,
                                );
                            }
                        } else if let Some(url) = obj.get("url").and_then(|v| v.as_str()) {
                            let filename = obj
                                .get("filename")
                                .and_then(|v| v.as_str())
                                .unwrap_or("file");
                            let media_type = obj.get("media_type").and_then(|v| v.as_str());
                            parts.push(
                                file_part_from_data_or_url(url, filename, media_type, proxy_url)
                                    .await?,
                            );
                        }
                    }
                    _ => {
                        if let Some(url) = obj.get("url").and_then(|v| v.as_str()) {
                            let filename = obj
                                .get("filename")
                                .and_then(|v| v.as_str())
                                .unwrap_or("file");
                            let media_type = obj.get("media_type").and_then(|v| v.as_str());
                            parts.push(
                                file_part_from_data_or_url(url, filename, media_type, proxy_url)
                                    .await?,
                            );
                        }
                    }
                }
            }
        }
        _ => warn!("Unsupported content type: {:?}", content),
    }

    if parts.is_empty() && !recognized_content {
        warn!(
            "Preserving unknown content instead of dropping: {}",
            serde_json::to_string(content).unwrap_or_default()
        );

        parts.push(json!({
            "type": "text",
            "text": serde_json::to_string(content).unwrap_or_default()
        }));
    }

    Ok(parts)
}

fn sanitize_inbound_text(text: &str) -> String {
    let mut cleaned = text.to_string();
    for tag in [
        "system-reminder",
        "system",
        "reminder",
        "context",
        "hidden",
        "instructions",
        "note",
    ] {
        let pattern = format!(r"(?is)<{tag}[^>]*>.*?</{tag}>");
        cleaned = regex::Regex::new(&pattern)
            .unwrap()
            .replace_all(&cleaned, "")
            .to_string();
    }

    cleaned.trim().to_string()
}

async fn build_frame(
    chat_id: &str,
    account: &Account,
    model: &str,
    messages: &[Value],
    model_prefix: &str,
    proxy_url: Option<&str>,
) -> Result<Value> {
    let model_slug = resolve_model(model);
    let selected_model = format!("{}{}", model_prefix, model_slug);
    debug!(
        "build_frame start: chat_id={}, model={}, selected_model={}, incoming_messages={}",
        chat_id,
        model,
        selected_model,
        summarize_frame_input(messages)
    );

    let mut use_messages = Vec::new();

    for msg in messages {
        let role = msg.get("role").and_then(|v| v.as_str()).unwrap_or("user");
        let is_trusted_proxy_message = msg
            .get("metadata")
            .and_then(|m| m.get("leech_proxy_tool_prompt"))
            .and_then(|v| v.as_bool())
            .unwrap_or(false);

        if role == "system" {
            debug!(
                "Preserving system message for upstream frame. trusted={} preview={}",
                is_trusted_proxy_message,
                summarize_content(msg.get("content"))
            );
        }

        let synthesized_tool_calls_text: Option<String> = if role == "assistant" {
            msg.get("tool_calls")
                .and_then(|v| v.as_array())
                .filter(|calls| !calls.is_empty())
                .map(|calls| {
                    calls
                        .iter()
                        .filter_map(|call| {
                            let name = call
                                .get("function")
                                .and_then(|f| f.get("name"))
                                .and_then(|v| v.as_str())?;
                            let raw_args = call
                                .get("function")
                                .and_then(|f| f.get("arguments"))
                                .and_then(|v| v.as_str())
                                .unwrap_or("{}");
                            let input: Value =
                                serde_json::from_str(raw_args).unwrap_or_else(|_| json!({}));
                            Some(format!(
                                "<tool_use>\n{}\n</tool_use>",
                                json!({ "name": name, "input": input })
                            ))
                        })
                        .collect::<Vec<_>>()
                        .join("\n")
                })
                .filter(|s| !s.is_empty())
        } else {
            None
        };

        let parts = match (&synthesized_tool_calls_text, msg.get("content")) {
            (Some(text), _) => {
                // opencode sends the assistant's own prior tool call back to us as
                // {"role":"assistant","content":"","tool_calls":[...]}. If we only
                // look at `content` here it's empty/null and the tool call info is
                // lost entirely -- the model then has no memory of ever having
                // called a tool on a later turn, only a "Tool result for call_X"
                // appearing with no context. Reconstruct it in the same <tool_use>
                // text format the model was taught, so history stays coherent.
                vec![json!({ "type": "text", "text": text.clone() })]
            }
            (None, Some(content)) if is_trusted_proxy_message => {
                // This message was constructed by leech-rs itself (the tool-call
                // prompt + serialized tool schemas), not typed by the end user.
                // Running it through sanitize_inbound_text() would treat our own
                // instructions as a prompt-injection attempt and could strip out
                // tool descriptions that happen to contain tag-like text.
                let text = content
                    .as_str()
                    .map(|s| s.to_string())
                    .unwrap_or_else(|| content.to_string());
                vec![json!({ "type": "text", "text": text })]
            }
            (None, Some(content)) => build_parts_from_content(content, proxy_url).await?,
            (None, None) => vec![json!({ "type": "text", "text": "" })],
        };
        if parts.is_empty()
            || parts.iter().all(|part| {
                part.get("type").and_then(|v| v.as_str()) == Some("text")
                    && part
                        .get("text")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .is_empty()
            })
        {
            debug!(
                "Dropping empty message after inbound sanitization: role={} source_summary={}",
                role,
                summarize_frame_input(std::slice::from_ref(msg))
            );
            continue;
        }

        use_messages.push(json!({
            "id": uuid::Uuid::new_v4().simple().to_string(),
            "role": role,
            "parts": parts,
            "metadata": {},
        }));
    }

    if use_messages.is_empty() {
        use_messages.push(json!({
            "id": uuid::Uuid::new_v4().simple().to_string(),
            "role": "user",
            "parts": [{ "type": "text", "text": "" }],
            "metadata": {},
        }));
    }

    // NOTE: this intentionally skips our own `leech_proxy_tool_prompt` message.
    // That message always contains "<tool_use>" / "Available tools:" (it's the
    // text we inject to *simulate* tool calling), so without this exclusion every
    // tool-enabled request flips agenticMode on and gets routed to use.ai's real
    // Agent product (source=agent_page), which has its own real system context
    // about its own real capabilities. That context contradicts our pretend-tools
    // instructions and is why the model says "I don't have access to your files"
    // instead of emitting a fake tool call. Only let *real* conversation content
    // (actual prior tool results, etc.) trigger agentic mode.
    debug!(
        "build_frame assembled messages: {}",
        summarize_frame_messages(&json!({ "messages": use_messages.clone() }))
    );
    let has_proxy_tool_prompt_context = messages.iter().any(|msg| {
        msg.get("metadata")
            .and_then(|m| m.get("leech_proxy_tool_prompt"))
            .and_then(|v| v.as_bool())
            .unwrap_or(false)
    });

    let agentic_mode = if has_proxy_tool_prompt_context {
        false
    } else {
        messages.iter().any(|msg| {
            let content = msg.get("content");
            match content {
                Some(Value::String(text)) => {
                    text.contains("<tool_use>")
                        || text.contains("tool_choice")
                        || text.contains("Available tools:")
                        || text.contains("function_call_output")
                        || text.contains("Tool result for ")
                }
                Some(Value::Array(items)) => {
                    items.iter().any(|item| item.to_string().contains("tool"))
                }
                Some(Value::Object(obj)) => serde_json::Value::Object(obj.clone())
                    .to_string()
                    .contains("tool"),
                _ => false,
            }
        })
    };

    debug!(
        "build_frame final flags: proxy_tool_prompt_context={}, agentic_mode={}, outgoing_count={}",
        has_proxy_tool_prompt_context,
        agentic_mode,
        use_messages.len()
    );
    Ok(json!({
        "chatId": chat_id,
        "userId": account.user_id,
        "email": account.email,
        "userType": "regular",
        "userEmail": account.email,
        "planType": "free",
        "subscriptionStatus": "inactive",
        "isFreemium": false,
        "isTestUser": false,
        "experimentCohort": "A",
        "cfModelsVariant": "OFF",
        "mixpanelUserId": uuid::Uuid::new_v4().to_string(),
        "deviceId": uuid::Uuid::new_v4().to_string(),
        "isWebSearchMode": false,
        "isDeepResearchMode": false,
        "isImageGenerationMode": false,
        "agenticMode": agentic_mode,
        "isStandaloneImageMode": false,
        "needsBlurPreview": false,
        "deepResearchProcessor": "pro-fast",
        "selectedModel": selected_model,
        "locale": "en",
        "userTimezone": "Europe/Zagreb",
        "userCountry": "Croatia (HR)",
        "messages": use_messages,
        "trigger": "submit-message",
        "source": if agentic_mode { "agent_page" } else { "chat_page" },
    }))
}

fn summarize_frame_input(messages: &[Value]) -> String {
    messages
        .iter()
        .enumerate()
        .map(|(idx, msg)| {
            let role = msg.get("role").and_then(|v| v.as_str()).unwrap_or("?");
            let trusted = msg
                .get("metadata")
                .and_then(|m| m.get("leech_proxy_tool_prompt"))
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            let has_tool_calls = msg
                .get("tool_calls")
                .and_then(|v| v.as_array())
                .map(|calls| !calls.is_empty())
                .unwrap_or(false);
            let preview = msg
                .get("content")
                .map(|v| v.to_string().chars().take(80).collect::<String>())
                .unwrap_or_default();
            format!(
                "{}:{}:trusted={}:tool_calls={}:{}",
                idx, role, trusted, has_tool_calls, preview
            )
        })
        .collect::<Vec<_>>()
        .join(" | ")
}
fn summarize_content(content: Option<&Value>) -> String {
    let text = match content {
        Some(Value::String(text)) => text.clone(),
        Some(Value::Array(items)) => items
            .iter()
            .filter_map(|item| item.get("text").and_then(|v| v.as_str()))
            .collect::<Vec<_>>()
            .join(" "),
        Some(Value::Object(obj)) => obj
            .get("text")
            .and_then(|v| v.as_str())
            .map(ToOwned::to_owned)
            .unwrap_or_else(|| content.unwrap().to_string()),
        Some(other) => other.to_string(),
        None => String::new(),
    };

    let mut preview = text.chars().take(160).collect::<String>();
    if text.chars().count() > 160 {
        preview.push_str("...");
    }
    preview.replace('\n', "\\n")
}

/// Detect a "premature intent" turn: the assistant announced work it will do
/// ("I'll fix…", "let me…", "I'm going to…") without producing any actual work
/// artifact (no fenced code block, no `<tool_use>`). These are the turns that
/// end early and force the user to type "continue". Requiring an intent lead-in
/// *and* the absence of work artifacts keeps false positives very low: genuine
/// completions and real tool calls never match.
fn looks_like_premature_intent(text: &str) -> bool {
    let lower = text.to_lowercase();
    if lower.contains("```") || lower.contains("<tool_use>") {
        return false;
    }
    let intent = [
        "i'll",
        "i will",
        "let me",
        "i'm going to",
        "i am going to",
        "i plan to",
        "i'm gonna",
        "i am gonna",
        "we'll plan the implementation",
        "let me read the remaining files directly to complete the picture",
        "let me start implementing",
    ];
    intent.iter().any(|p| lower.contains(p))
}

// ---------- Streaming completion ----------

/// Compute an exponential-backoff delay for retry attempt `attempt` (1-indexed:
/// the first retry is attempt 1). Delay grows as `base * factor^(attempt-1)`,
/// capped at `max`, with equal jitter applied so the actual wait lands in
/// `[cap/2, cap]`. Jitter prevents thundering-herd retries against use.ai when
/// several requests get rate-limited simultaneously.
fn backoff_duration(base: Duration, max: Duration, factor: f64, attempt: usize) -> Duration {
    let exp = attempt.saturating_sub(1) as f64;
    let raw = base.as_millis() as f64 * factor.powf(exp);
    let cap = raw.min(max.as_millis() as f64).max(0.0);
    let half = cap / 2.0;
    let mut rng = rand::thread_rng();
    let jitter = rng.gen_range(0.0..=half);
    Duration::from_millis((half + jitter) as u64)
}

pub async fn stream_completion(
    model: &str,
    messages: &[Value],
    proxy_url: Option<&str>,
    account: Account,
) -> BoxStream<'static, Result<String>> {
    let cfg = Config::load().unwrap_or_default();
    let chat_id = uuid::Uuid::new_v4().to_string();
    // Base URI without token — the token is added in attempt() after
    // refresh_session, because use.ai's agent gateway authenticates via
    // a ?token=<JWT> query parameter (from POST /api/auth/token), NOT via
    // Authorization header or cookie alone.
    let uri_base = format!(
        "{}/{chat_id}?userId={}&userType=regular&userEmail={}&planType=free&isTestUser=false",
        cfg.direct.ws_agent_base, account.user_id, account.email,
    );

    let open_timeout = Duration::from_secs(cfg.direct.ws_open_timeout_sec);
    let idle_timeout = Duration::from_secs(cfg.direct.ws_idle_timeout_sec);
    let retries = cfg.direct.direct_ws_retries.max(1);
    let model_prefix = cfg.direct.model_prefix.clone();
    let auto_continue = cfg.direct.auto_continue;
    let auto_continue_max = cfg.direct.auto_continue_max;
    let debug_protocol = cfg.logging.debug_protocol;
    let backoff_base = Duration::from_millis(cfg.direct.direct_ws_backoff_base_ms);
    let backoff_max = Duration::from_millis(cfg.direct.direct_ws_backoff_max_ms);
    let backoff_factor = cfg.direct.direct_ws_backoff_factor;
    let per_proxy_cooldown = Duration::from_millis(cfg.direct.use_ai_per_proxy_cooldown_ms);

    // Build the ordered list of proxy assignments to try. We prefer the
    // supplied proxy for the first `retries` attempts, then fall back to
    // direct egress as a last resort (mirrors the original behavior).
    let mut attempt_proxies: Vec<Option<String>> = Vec::with_capacity(retries as usize + 1);
    for _ in 0..retries {
        attempt_proxies.push(proxy_url.map(String::from));
    }
    if proxy_url.is_some() {
        attempt_proxies.push(None);
    }

    // Owned captures for the 'static retry stream.
    let messages_owned = messages.to_vec();
    let account_owned = account.clone();
    let model_owned = model.to_string();

    // Unified exponential-backoff retry stream. This retries BOTH setup
    // failures (attempt() returns Err) and in-stream errors that occur before
    // any content has been emitted to the client — e.g. the mid-stream
    // `rate-limit-error` frame from use.ai. Once a single Ok chunk has been
    // yielded we can no longer restart transparently (the SSE body has begun),
    // so later errors propagate as-is.
    let merged = async_stream::stream! {
        let mut emitted_any = false;
        let mut last_err: Option<anyhow::Error> = None;
        let mut current: Option<BoxStream<'static, Result<String>>> = None;
        let mut step: usize = 0;

        loop {
            if current.is_none() {
                if step >= attempt_proxies.len() {
                    let err = last_err.unwrap_or_else(|| anyhow!("all retries failed"));
                    yield Err(err);
                    return;
                }
                // Backoff between attempts; skip the very first try.
                if step > 0 {
                    let delay = backoff_duration(backoff_base, backoff_max, backoff_factor, step);
                    debug!(
                        "direct retry {}/{} after {:?} (emitted_any={})",
                        step + 1,
                        attempt_proxies.len(),
                        delay,
                        emitted_any
                    );
                    tokio::time::sleep(delay).await;
                }
                let attempt_proxy = attempt_proxies[step].clone();
                step += 1;
                match attempt(
                    uri_base.clone(),
                    chat_id.clone(),
                    account_owned.clone(),
                    model_owned.clone(),
                    messages_owned.clone(),
                    attempt_proxy,
                    model_prefix.clone(),
                    open_timeout,
                    idle_timeout,
                    auto_continue,
                    auto_continue_max,
                    debug_protocol,
                    per_proxy_cooldown,
                )
                .await
                {
                    Ok(s) => current = Some(s),
                    Err(e) => {
                        last_err = Some(e);
                        continue;
                    }
                }
            }

            match current.as_mut().unwrap().next().await {
                Some(Ok(item)) => {
                    if !item.is_empty() {
                        emitted_any = true;
                    }
                    yield Ok(item);
                }
                Some(Err(e)) => {
                    last_err = Some(e);
                    current.take();
                    if emitted_any {
                        // Content already streamed — cannot restart the SSE
                        // body, so surface the error to the client.
                        let err = last_err.unwrap();
                        yield Err(err);
                        return;
                    }
                    // A per-IP rate limit cannot be escaped by retrying: use.ai
                    // binds the session to the account's signup Tor exit, so
                    // every retry would hit the same IP that just rate-limited
                    // us (only wasting the per-proxy cooldown). Propagate it
                    // immediately so the caller returns a real error and the
                    // client retries the whole request with a fresh account /
                    // exit. Other pre-content errors still loop-and-retry.
                    if is_stream_rate_limit_error(last_err.as_ref().unwrap()) {
                        let err = last_err.unwrap();
                        yield Err(err);
                        return;
                    }
                    // No content emitted yet: loop and retry with backoff.
                }
                None => return, // clean end of stream
            }
        }
    };

    #[allow(clippy::too_many_arguments)]
    async fn attempt(
        uri_base: String,
        chat_id: String,
        account: Account,
        model: String,
        messages: Vec<Value>,
        proxy_url: Option<String>,
        model_prefix: String,
        open_timeout: Duration,
        idle_timeout: Duration,
        auto_continue: bool,
        auto_continue_max: u32,
        debug_protocol: bool,
        per_proxy_cooldown: Duration,
    ) -> Result<BoxStream<'static, Result<String>>> {
        // Use the account's original signup proxy for refresh and WS — use.ai
        // binds the session to the signup IP. Connecting from a different Tor
        // exit causes AUTH_REQUIRED (4001) on the agent WebSocket.
        let account_proxy = account.proxy_url.as_deref().or(proxy_url.as_deref());

        // Space out consecutive sends through the same Tor exit so use.ai's
        // per-IP rate limit doesn't trip before we even open the WS. Done
        // before refresh_session because that also hits use.ai from this IP.
        let proxy_key = account_proxy.unwrap_or("direct");
        await_per_proxy_cooldown(proxy_key, per_proxy_cooldown).await;

        // Refresh the session cookies before connecting. The
        // __Secure-better-auth.session_data JWT has a 60-second TTL, so
        // pooled accounts must be refreshed or the agent gateway rejects
        // with AUTH_REQUIRED (4001).
        let (cookie_header, token, app_token) =
            match refresh_session(&account, account_proxy).await {
                Ok((c, t, at)) => (c, t, at),
                Err(e) => {
                    debug!("Session refresh failed ({}), using original cookies", e);
                    (
                        account.cookie_header.clone(),
                        account.token.clone(),
                        account.app_token.clone(),
                    )
                }
            };

        // Build the full WS URI with the JWT token as a query parameter.
        // use.ai's agent gateway authenticates via ?token=<JWT> (from the
        // /api/auth/token endpoint), NOT via Authorization header. The
        // browser does: new WebSocket(baseUrl + "/" + chatId + "?token=...")
        // and also appends &app_token=<value>.
        let mut uri = uri_base.clone();
        if !token.is_empty() {
            uri.push_str(&format!("&token={}", token));
        }
        if let Some(app_token) = app_token
            .as_ref()
            .filter(|at| !at.is_empty())
        {
            uri.push_str(&format!("&app_token={}", app_token));
        }

        let mut ws_stream = connect_websocket_with_proxy(
            &uri,
            account_proxy,
            open_timeout,
            None,
            Some(&cookie_header),
            debug_protocol,
        )
        .await?;

        let frame = build_frame(
            &chat_id,
            &account,
            &model,
            &messages,
            &model_prefix,
            account_proxy,
        )
        .await?;
        if debug_protocol {
            debug!(
                "Upstream frame messages: {}",
                summarize_frame_messages(&frame)
            );
        }
        ws_stream.send(Message::Text(frame.to_string())).await?;

        let mut filter = InjectionFilter::new();
        let stream = async_stream::stream! {
            // Running conversation history. The initial frame was already sent
            // from `messages`; on an auto-continue we append the just-finished
            // assistant segment + a "Continue." user turn and resend the full
            // history as a new submit-message frame on this same WebSocket.
            let mut conversation = messages.clone();
            // Accumulates the visible text of the current segment so the
            // premature-intent heuristic can inspect it on `finish`.
            let mut assistant_text = String::new();
            let mut continuations = 0u32;

            loop {
                let msg = match tokio::time::timeout(idle_timeout, ws_stream.next()).await {
                    Ok(Some(Ok(msg))) => msg,
                    Ok(Some(Err(e))) => {
                        yield Err(anyhow!("WebSocket error: {}", e));
                        break;
                    }
                    Ok(None) => break,
                    Err(_) => {
                        yield Err(anyhow!("WebSocket idle timeout"));
                        break;
                    }
                };

                if let Message::Text(text) = msg {
                    // Log every raw text frame for protocol debugging. Gated
                    // behind debug_protocol because this fires on every frame
                    // of every active stream and floods log sinks (e.g. the
                    // Railway 500 logs/sec rate limit).
                    if debug_protocol {
                        debug!(
                            "WS recv text ({} bytes): {}",
                            text.len(),
                            &text[..text.len().min(300)]
                        );
                    }
                    if let Ok(val) = serde_json::from_str::<Value>(&text) {
                        if let Some(chunk) = val.get("chunk") {
                            if let Some(delta) = chunk.get("delta").and_then(|d| d.as_str()) {
                                let safe = filter.feed(delta);
                                assistant_text.push_str(&safe);
                                if !safe.is_empty() {
                                    yield Ok(safe);
                                }
                            }
                        }

                        if let Some(typ) = val.get("type").and_then(|t| t.as_str()) {
                            if typ == "finish" || typ == "stream-complete" {
                                // If the turn looks like a premature intent
                                // announcement (no work artifact + future-tense
                                // lead-in), send a "Continue." follow-up on the
                                // same WS and keep streaming, up to the cap.
                                if auto_continue
                                    && continuations < auto_continue_max
                                    && !assistant_text.is_empty()
                                    && looks_like_premature_intent(&assistant_text)
                                {
                                    continuations += 1;
                                    // Drain filter state so it doesn't carry
                                    // into the next segment. flush() is
                                    // idempotent, so the end-of-block flush
                                    // below stays a no-op on the break paths.
                                    let tail = filter.flush();
                                    if !tail.is_empty() {
                                        yield Ok(tail);
                                    }
                                    debug!(
                                        "Auto-continue {}/{}: premature intent detected, sending \"Continue.\"",
                                        continuations, auto_continue_max
                                    );
                                    conversation.push(json!({
                                        "role": "assistant",
                                        "content": assistant_text.clone(),
                                    }));
                                    conversation.push(json!({
                                        "role": "user",
                                        "content": "Continue.",
                                    }));
                                    assistant_text.clear();

                                    let cont_frame = match build_frame(
                                        &chat_id,
                                        &account,
                                        &model,
                                        &conversation,
                                        &model_prefix,
                                        account.proxy_url.as_deref().or(proxy_url.as_deref()),
                                    )
                                    .await
                                    {
                                        Ok(f) => f,
                                        Err(e) => {
                                            yield Err(anyhow!(
                                                "auto-continue build_frame failed: {}",
                                                e
                                            ));
                                            break;
                                        }
                                    };
                                    if let Err(e) = ws_stream
                                        .send(Message::Text(cont_frame.to_string()))
                                        .await
                                    {
                                        yield Err(anyhow!(
                                            "auto-continue send failed: {}",
                                            e
                                        ));
                                        break;
                                    }
                                    continue;
                                }
                                break;
                            }
                            if typ == "rate-limit-error" {
                                yield Err(anyhow::Error::from(RateLimitError));
                                break;
                            }
                        }
                    }
                } else if let Message::Close(frame) = msg {
                    if debug_protocol {
                        debug!("WS close frame received: {:?}", frame);
                    }
                    break;
                }
            }

            let tail = filter.flush();
            if !tail.is_empty() {
                yield Ok(tail);
            }
        };

        Ok(Box::pin(stream) as BoxStream<'static, Result<String>>)
    }

    Box::pin(merged)
}

fn summarize_frame_messages(frame: &Value) -> String {
    frame
        .get("messages")
        .and_then(|v| v.as_array())
        .map(|messages| {
            messages
                .iter()
                .enumerate()
                .map(|(idx, msg)| {
                    let role = msg.get("role").and_then(|v| v.as_str()).unwrap_or("?");
                    let text = msg
                        .get("parts")
                        .and_then(|v| v.as_array())
                        .map(|parts| {
                            parts
                                .iter()
                                .filter_map(|part| part.get("text").and_then(|v| v.as_str()))
                                .collect::<Vec<_>>()
                                .join(" ")
                        })
                        .unwrap_or_default();
                    let preview = text
                        .chars()
                        .take(80)
                        .collect::<String>()
                        .replace('\n', "\\n");
                    format!("{}:{}:{}", idx, role, preview)
                })
                .collect::<Vec<_>>()
                .join(" | ")
        })
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn peek_stream_start_surfaces_fatal_rate_limit_as_err() {
        // A stream whose first item is a RateLimitError (no content emitted)
        // must be surfaced as Err so callers return a real 429 instead of an
        // empty "stop" completion.
        let fatal: BoxStream<'static, Result<String>> = Box::pin(futures::stream::iter(
            vec![Err(anyhow::Error::from(RateLimitError))],
        ));
        let result = peek_stream_start(fatal).await;
        let err = match result {
            Err(e) => e,
            Ok(_) => panic!("expected fatal rate-limit error, got a stream"),
        };
        assert!(is_stream_rate_limit_error(&err));
    }

    #[tokio::test]
    async fn peek_stream_start_prepends_content_and_keeps_stream() {
        // A stream that opens with content must be returned intact with the
        // peeked item prepended, so normal consumption is unchanged.
        let s: BoxStream<'static, Result<String>> = Box::pin(futures::stream::iter(vec![
            Ok("hello".to_string()),
            Ok(" world".to_string()),
        ]));
        let mut stream = peek_stream_start(s).await.expect("content stream should be Ok");
        let mut out = String::new();
        while let Some(chunk) = stream.next().await {
            out.push_str(&chunk.unwrap());
        }
        assert_eq!(out, "hello world");
    }

    #[tokio::test]
    async fn peek_stream_start_passes_through_non_rate_limit_error() {
        // A non-rate-limit first error is prepended back (not surfaced as Err)
        // so existing [ERROR]-delta behavior for other failures is preserved.
        let s: BoxStream<'static, Result<String>> = Box::pin(futures::stream::iter(vec![
            Err(anyhow!("connection refused")),
            Ok("after".to_string()),
        ]));
        let mut stream = peek_stream_start(s).await.expect("non-rate-limit err is not fatal");
        let first = stream.next().await.unwrap();
        assert!(first.is_err());
        assert!(!is_stream_rate_limit_error(&first.unwrap_err()));
    }

    #[test]
    fn backoff_grows_exponentially_and_caps_at_max() {
        let base = Duration::from_millis(500);
        let max = Duration::from_millis(8000);
        let factor = 2.0;

        // Attempt 1 -> cap = 500ms, so delay in [250, 500].
        let d1 = backoff_duration(base, max, factor, 1).as_millis();
        assert!((250..=500).contains(&d1), "attempt 1 out of range: {d1}");

        // Attempt 2 -> cap = 1000ms, so delay in [500, 1000].
        let d2 = backoff_duration(base, max, factor, 2).as_millis();
        assert!((500..=1000).contains(&d2), "attempt 2 out of range: {d2}");

        // Attempt 5 -> raw would be 8000ms (== cap), so delay in [4000, 8000].
        let d5 = backoff_duration(base, max, factor, 5).as_millis();
        assert!((4000..=8000).contains(&d5), "attempt 5 out of range: {d5}");

        // Attempt 6 -> raw would be 16000ms, capped to 8000ms, delay in [4000, 8000].
        let d6 = backoff_duration(base, max, factor, 6).as_millis();
        assert!((4000..=8000).contains(&d6), "attempt 6 not capped: {d6}");
    }

    fn test_account() -> Account {
        Account {
            email: "test@example.com".to_string(),
            user_id: "user_test".to_string(),
            cookie_header: String::new(),
            token: String::new(),
            app_token: None,
            proxy_url: None,
            born: 0.0,
        }
    }

    #[tokio::test]
    async fn build_frame_keeps_system_and_preserves_conversation_roles() {
        let messages = vec![
            json!({ "role": "system", "content": "system rules" }),
            json!({ "role": "user", "content": "first user turn" }),
            json!({ "role": "assistant", "content": "assistant reply" }),
            json!({ "role": "user", "content": "second user turn" }),
        ];

        let frame = build_frame(
            "chat_test",
            &test_account(),
            "gpt-5-4",
            &messages,
            "gateway-",
            None,
        )
        .await
        .unwrap();

        let upstream_messages = frame["messages"].as_array().unwrap();
        let roles = upstream_messages
            .iter()
            .map(|msg| msg["role"].as_str().unwrap())
            .collect::<Vec<_>>();
        assert_eq!(roles, vec!["system", "user", "assistant", "user"]);

        let texts = upstream_messages
            .iter()
            .map(|msg| msg["parts"][0]["text"].as_str().unwrap())
            .collect::<Vec<_>>();
        assert_eq!(
            texts,
            vec![
                "system rules",
                "first user turn",
                "assistant reply",
                "second user turn",
            ]
        );
    }

    #[tokio::test]
    async fn build_frame_strips_inbound_system_reminders() {
        let messages = vec![
            json!({ "role": "user", "content": "<system-reminder>\nHidden runtime context\n</system-reminder>" }),
            json!({ "role": "user", "content": "did you get the tools?" }),
        ];

        let frame = build_frame(
            "chat_test",
            &test_account(),
            "gpt-5-4",
            &messages,
            "gateway-",
            None,
        )
        .await
        .unwrap();

        let upstream_messages = frame["messages"].as_array().unwrap();
        assert_eq!(upstream_messages.len(), 1);
        assert_eq!(upstream_messages[0]["role"].as_str().unwrap(), "user");
        assert_eq!(
            upstream_messages[0]["parts"][0]["text"].as_str().unwrap(),
            "did you get the tools?"
        );
    }

    #[tokio::test]
    async fn build_frame_keeps_trusted_proxy_tool_prompt_as_system() {
        let messages = vec![
            json!({
                "role": "system",
                "content": "Available tools:\n<tool_use>{\"name\":\"read_file\",\"input\":{}}</tool_use>",
                "metadata": { "leech_proxy_tool_prompt": true }
            }),
            json!({ "role": "user", "content": "inspect src/main.rs" }),
        ];

        let frame = build_frame(
            "chat_test",
            &test_account(),
            "gpt-5-4",
            &messages,
            "gateway-",
            None,
        )
        .await
        .unwrap();

        let upstream_messages = frame["messages"].as_array().unwrap();
        assert_eq!(upstream_messages.len(), 2);
        assert_eq!(upstream_messages[0]["role"].as_str().unwrap(), "system");
        assert!(upstream_messages[0]["parts"][0]["text"]
            .as_str()
            .unwrap()
            .contains("Available tools:"));
        assert_eq!(upstream_messages[1]["role"].as_str().unwrap(), "user");
    }

    #[tokio::test]
    async fn build_frame_keeps_regular_system_prompt_as_system() {
        let messages = vec![
            json!({
                "role": "system",
                "content": "You are OpenCode. You and the user share the same workspace."
            }),
            json!({ "role": "user", "content": "inspect src/main.rs" }),
        ];

        let frame = build_frame(
            "chat_test",
            &test_account(),
            "gpt-5-4",
            &messages,
            "gateway-",
            None,
        )
        .await
        .unwrap();

        let upstream_messages = frame["messages"].as_array().unwrap();
        assert_eq!(upstream_messages.len(), 2);
        assert_eq!(upstream_messages[0]["role"].as_str().unwrap(), "system");
        assert!(upstream_messages[0]["parts"][0]["text"]
            .as_str()
            .unwrap()
            .contains("You are OpenCode"));
        assert_eq!(upstream_messages[1]["role"].as_str().unwrap(), "user");
    }

    #[test]
    fn premature_intent_matches_reported_announcement() {
        // The exact case from the bug report: model says what it will do, then
        // stops the turn instead of doing the work.
        let text = "You're right - that view is basically unstyled raw HTML. \
                    I'll fix the UI styling and spacing, then build and release a patch.";
        assert!(looks_like_premature_intent(text));
    }

    #[test]
    fn premature_intent_matches_other_lead_ins() {
        assert!(looks_like_premature_intent(
            "I'm going to refactor src/main.rs next."
        ));
        assert!(looks_like_premature_intent(
            "Let me update the config and redeploy."
        ));
        assert!(looks_like_premature_intent("I will add tests for this."));
    }

    #[test]
    fn premature_intent_rejects_code_block() {
        // A turn that already produced a code block is real work, not a preamble.
        let text = "Here is the fix:\n```rust\nfn main() {}\n```";
        assert!(!looks_like_premature_intent(text));
    }

    #[test]
    fn premature_intent_rejects_tool_use() {
        // Real agentic tool calls must never be auto-continued; the client
        // handles them.
        let text = "<tool_use>\n{\"name\":\"edit\",\"input\":{\"path\":\"a.rs\"}}\n</tool_use>";
        assert!(!looks_like_premature_intent(text));
    }

    #[test]
    fn premature_intent_rejects_genuine_completion() {
        // No intent lead-in -> the model actually finished.
        assert!(!looks_like_premature_intent("Done. The build passed."));
        assert!(!looks_like_premature_intent(""));
    }
}
