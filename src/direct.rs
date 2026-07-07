//! Headless WebSocket account creation and streaming.

use anyhow::{anyhow, Result};
use base64::{engine::general_purpose, Engine as _};
use futures::{SinkExt, StreamExt};
use reqwest::cookie::{CookieStore, Jar};
use reqwest::Client;
use serde_json::{json, Value};
use std::sync::Arc;
use std::time::Duration;
use tokio::net::TcpStream;
use tokio::time::timeout;
use tokio_socks::tcp::Socks5Stream;
use tokio_tungstenite::{client_async_tls_with_config, MaybeTlsStream, WebSocketStream};
use tungstenite::{client::IntoClientRequest, protocol::WebSocketConfig, Message};

use crate::account_pool::Account;
use crate::config::Config;
use crate::filter::InjectionFilter;
use crate::models::resolve_model;
use crate::utils::{gen_email, now_secs};
use tracing::{debug, error, info, warn};

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
        debug!(
            "Account creation attempt {} using {}",
            idx + 1,
            proxy_desc
        );

        let mut retry_count = 0;
        while retry_count < 3 {
            match create_account_once(proxy).await {
                Ok(account) => return Ok(account),
                Err(e) => {
                    if is_rate_limit_error(&e) {
                        let wait = Duration::from_secs(2u64.pow(retry_count + 1));
                        warn!("Received 429 using {}, waiting {:?} before retry", proxy_desc, wait);
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

fn build_client_with_jar(proxy_url: Option<&str>) -> Result<(Client, Arc<Jar>)> {
    let jar = Arc::new(Jar::default());
    let client_builder = Client::builder()
        .timeout(Duration::from_secs(30))
        .cookie_provider(jar.clone())
        .user_agent(USER_AGENT)
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

    // 1. email-login
    let resp = client
        .post(&format!("{}/email-login", auth_base))
        .json(&json!({ "email": email }))
        .send()
        .await
        .map_err(|e| {
            error!("email-login request failed: {:?}", e);
            anyhow!("email-login request failed: {}", e)
        })?;
    if !resp.status().is_success() {
        let status = resp.status();
        let text = resp.text().await.unwrap_or_default();
        error!("email-login failed: {} - {}", status, text);
        anyhow::bail!("email-login failed: {} - {}", status, text);
    }

    // 2. sign-in/credentials
    let resp = client
        .post(&format!("{}/sign-in/credentials", auth_base))
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

    // 3. get-session
    let resp = client
        .get(&format!("{}/get-session", auth_base))
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
    // NEW: read the JWT from headers
    let jwt = resp
        .headers()
        .get("set-auth-jwt")
        .and_then(|h| h.to_str().ok())
        .unwrap_or_default()
        .to_string();

    let body: Value = resp.json().await?;
    let user_id = body["user"]["id"]
        .as_str()
        .ok_or_else(|| anyhow!("user id not found"))?
        .to_string();

    let url = "https://api.use.ai".parse()?;
    let cookie_header = jar
        .cookies(&url)
        .and_then(|value| value.to_str().ok().map(ToOwned::to_owned))
        .unwrap_or_default();

    info!("Account created: {} (user: {})", email, user_id.chars().take(8).collect::<String>());
    Ok(Account {
        email,
        user_id,
        cookie_header,
        token: jwt,
        born: now_secs(),
    })
}

// ---------- WebSocket connection with SOCKS5 ----------

async fn connect_websocket_with_proxy(
    uri: &str,
    proxy_url: Option<&str>,
    open_timeout: Duration,
) -> Result<WebSocketStream<MaybeTlsStream<TcpStream>>> {
    let mut request = uri.into_client_request()?;
    request.headers_mut().insert(
        "User-Agent",
        tungstenite::http::HeaderValue::from_static(USER_AGENT),
    );

    if let Some(proxy) = proxy_url {
        let (host, port) = parse_socks5_proxy(proxy)?;
        let target_uri = tungstenite::http::Uri::try_from(uri)?;
        let target_host = target_uri.host().unwrap_or("agents.use.ai");
        let target_port = target_uri.port_u16().unwrap_or(443);

        let sock_stream = timeout(
            open_timeout,
            Socks5Stream::connect((host, port), (target_host, target_port)),
        )
        .await
        .map_err(|_| anyhow!("SOCKS connect timeout"))?
        .map_err(|e| anyhow!("SOCKS connection failed: {}", e))?;

        let config = WebSocketConfig::default();
        let (ws, _) =
            client_async_tls_with_config(request, sock_stream.into_inner(), Some(config), None)
                .await?;
        Ok(ws)
    } else {
        let config = WebSocketConfig::default();
        let (ws, _) = timeout(
            open_timeout,
            tokio_tungstenite::connect_async_with_config(request, Some(config), true),
        )
        .await
        .map_err(|_| anyhow!("WebSocket open timeout"))??;
        Ok(ws)
    }
}

fn parse_socks5_proxy(proxy: &str) -> Result<(&str, u16)> {
    if !proxy.starts_with("socks5h://") && !proxy.starts_with("socks5://") {
        anyhow::bail!("only socks5 proxies supported for WebSocket");
    }
    let host_port = proxy
        .split("://")
        .nth(1)
        .ok_or_else(|| anyhow!("invalid proxy URL"))?;
    let (host, port) = host_port
        .rsplit_once(':')
        .ok_or_else(|| anyhow!("proxy requires port"))?;
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
        let media_type = media_type_from_data_uri_header(&header, &guessed_media_type);
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

    match content {
        Value::String(text) => {
            if !text.is_empty() {
                parts.push(json!({ "type": "text", "text": text }));
            }
        }
        Value::Object(obj) => {
            if let Some(text) = obj.get("text").and_then(|v| v.as_str()) {
                if !text.is_empty() {
                    parts.push(json!({ "type": "text", "text": text }));
                }
            }

            if let Some(image_data) = obj.get("image").and_then(|v| v.as_str()) {
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
                        if let Some(text) = obj.get("text").and_then(|v| v.as_str()) {
                            if !text.is_empty() {
                                parts.push(json!({ "type": "text", "text": text }));
                            }
                        }
                    }
                    Some("image_url") => {
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
                                    file_part_from_data_or_url(url, filename, media_type, proxy_url)
                                        .await?,
                                );
                            } else if let Some(data) =
                                file_obj.get("data").and_then(|v| v.as_str())
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

    if parts.is_empty() {
        parts.push(json!({ "type": "text", "text": "" }));
    }

    Ok(parts)
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

    let has_structured_content = messages.iter().any(|msg| {
        matches!(
            msg.get("content"),
            Some(Value::Object(_)) | Some(Value::Array(_))
        )
    });

    let mut use_messages = Vec::new();

    if !has_structured_content {
        let mut parts = Vec::new();
        for msg in messages {
            if let Some(content) = msg.get("content").and_then(|c| c.as_str()) {
                parts.push(json!({
                    "type": "text",
                    "text": content,
                }));
            }
        }

        if parts.is_empty() {
            parts.push(json!({
                "type": "text",
                "text": "Hello",
            }));
        }

        use_messages.push(json!({
            "id": uuid::Uuid::new_v4().simple().to_string(),
            "role": "user",
            "parts": parts,
            "metadata": {},
        }));
    } else {
        for msg in messages {
            let role = msg.get("role").and_then(|v| v.as_str()).unwrap_or("user");
            let parts = match msg.get("content") {
                Some(content) => build_parts_from_content(content, proxy_url).await?,
                None => vec![json!({ "type": "text", "text": "" })],
            };

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
    }

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
        "agenticMode": false,
        "isStandaloneImageMode": false,
        "needsBlurPreview": false,
        "deepResearchProcessor": "pro-fast",
        "selectedModel": selected_model,
        "locale": "en",
        "userTimezone": "Europe/Zagreb",
        "userCountry": "Croatia (HR)",
        "messages": use_messages,
        "trigger": "submit-message",
        "source": "chat_page",
    }))
}

// ---------- Streaming completion ----------

pub async fn stream_completion(
    model: &str,
    messages: &[Value],
    proxy_url: Option<&str>,
    account: Account,
) -> impl futures::Stream<Item = Result<String>> {
    use futures::stream::{self, BoxStream};

    let cfg = Config::load().unwrap_or_default();
    let chat_id = uuid::Uuid::new_v4().to_string();
    let uri = format!(
        "{}/{chat_id}?userId={}&userType=regular&userEmail={}&planType=free&isTestUser=false&token={}",
        cfg.direct.ws_agent_base,
        account.user_id,
        account.email,
        account.token,
    );

    let open_timeout = Duration::from_secs(cfg.direct.ws_open_timeout_sec);
    let idle_timeout = Duration::from_secs(cfg.direct.ws_idle_timeout_sec);
    let retries = cfg.direct.direct_ws_retries.max(1);
    let model_prefix = cfg.direct.model_prefix.clone();

    async fn attempt(
        uri: String,
        chat_id: String,
        account: Account,
        model: String,
        messages: Vec<Value>,
        proxy_url: Option<String>,
        model_prefix: String,
        open_timeout: Duration,
        idle_timeout: Duration,
    ) -> Result<BoxStream<'static, Result<String>>> {
        let mut ws_stream =
            connect_websocket_with_proxy(&uri, proxy_url.as_deref(), open_timeout).await?;

        let frame = build_frame(
            &chat_id,
            &account,
            &model,
            &messages,
            &model_prefix,
            proxy_url.as_deref(),
        )
        .await?;
        ws_stream.send(Message::Text(frame.to_string())).await?;

        let mut filter = InjectionFilter::new();
        let stream = async_stream::stream! {
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
                    if let Ok(val) = serde_json::from_str::<Value>(&text) {
                        if let Some(chunk) = val.get("chunk") {
                            if let Some(delta) = chunk.get("delta").and_then(|d| d.as_str()) {
                                let safe = filter.feed(delta);
                                if !safe.is_empty() {
                                    yield Ok(safe);
                                }
                            }
                        }

                        if let Some(typ) = val.get("type").and_then(|t| t.as_str()) {
                            if typ == "finish" || typ == "stream-complete" {
                                break;
                            }
                            if typ == "rate-limit-error" {
                                yield Err(anyhow!("rate limit error"));
                                break;
                            }
                        }
                    }
                } else if let Message::Close(_) = msg {
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

    let mut last_err = None;
    let mut attempts = Vec::new();
    for _ in 1..=retries {
        attempts.push(proxy_url.map(String::from));
    }
    if proxy_url.is_some() {
        attempts.push(None);
    }

    for attempt_proxy in attempts {
        match attempt(
            uri.clone(),
            chat_id.clone(),
            account.clone(),
            model.to_string(),
            messages.to_vec(),
            attempt_proxy,
            model_prefix.clone(),
            open_timeout,
            idle_timeout,
        )
        .await
        {
            Ok(stream) => return stream,
            Err(e) => {
                last_err = Some(e);
                tokio::time::sleep(Duration::from_millis(500)).await;
            }
        }
    }

    let err = last_err.unwrap_or_else(|| anyhow!("all retries failed"));
    Box::pin(stream::once(async move { Err(err) }))
}

pub async fn complete_completion(
    model: &str,
    messages: &[Value],
    proxy_url: Option<&str>,
    account: Account,
) -> Result<String> {
    let mut stream = stream_completion(model, messages, proxy_url, account).await;
    let mut parts = Vec::new();
    while let Some(item) = stream.next().await {
        match item {
            Ok(text) => parts.push(text),
            Err(e) => anyhow::bail!("stream error: {}", e),
        }
    }
    let reply = parts.concat();
    if reply.is_empty() {
        anyhow::bail!("empty reply");
    }
    Ok(reply)
}
