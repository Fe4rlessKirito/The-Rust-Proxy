//! Headless WebSocket account creation and streaming.

use anyhow::{anyhow, Result};
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
    let cfg = Config::default();
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
        token: "".to_string(),
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

// ---------- Streaming completion ----------

fn build_frame(
    chat_id: &str,
    account: &Account,
    model: &str,
    messages: &[Value],
    model_prefix: &str,
) -> Value {
    let model_slug = resolve_model(model);
    let selected_model = format!("{}{}", model_prefix, model_slug);

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

    let use_messages = vec![json!({
        "id": uuid::Uuid::new_v4().simple().to_string(),
        "role": "user",
        "parts": parts,
        "metadata": {},
    })];

    json!({
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
    })
}

pub async fn stream_completion(
    model: &str,
    messages: &[Value],
    proxy_url: Option<&str>,
    account: Account,
) -> impl futures::Stream<Item = Result<String>> {
    use futures::stream::{self, BoxStream};

    let cfg = Config::default();
    let chat_id = uuid::Uuid::new_v4().to_string();
    let uri = format!(
        "{}/{chat_id}?userId={}&userType=regular&userEmail={}&planType=free&isTestUser=false",
        cfg.direct.ws_agent_base,
        account.user_id,
        account.email,
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

        let frame = build_frame(&chat_id, &account, &model, &messages, &model_prefix);
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
    for _ in 1..=retries {
        match attempt(
            uri.clone(),
            chat_id.clone(),
            account.clone(),
            model.to_string(),
            messages.to_vec(),
            proxy_url.map(String::from),
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
