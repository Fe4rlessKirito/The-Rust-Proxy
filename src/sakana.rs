//! Sakana provider integration.

use anyhow::{anyhow, Context, Result};
use futures::stream;
use futures::stream::BoxStream;
use futures::StreamExt;
use reqwest::cookie::Jar;
use reqwest::{multipart, Client};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::fs;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio::sync::Mutex;

const USER_AGENT: &str =
    "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/144.0.0.0 Safari/537.36";
const FIREBASE_API_KEY: &str = "AIzaSyBIJuyUokxGiETY0Nu3hQNC1dMadHyf_I4";
const FIREBASE_TENANT: &str = "sakana-talk-prd-pvl72";
const SAKANA_CHAT_BASE: &str = "https://chat.sakana.ai";
const SAKANA_API_URL: &str = "https://api.sakana.ai/v1/chat/completions";
const SAKANA_POOL_FILE: &str = "sakana_pool.json";
const SAKANA_POOL_MAX: usize = 50;
const SAKANA_ID_TOKEN_SLACK_SEC: u64 = 60;
const SAKANA_SESSION_TTL_SEC: u64 = 23 * 60 * 60;
const SAKANA_COOLOFF_SEC: u64 = 10 * 60;
const SAKANA_POOL_RETRIES: usize = 3;

#[derive(Debug, Clone, Serialize, Deserialize)]
struct SakanaPoolEntry {
    uid: String,
    refresh_token: String,
    id_token: String,
    id_token_exp: u64,
    session_cookie: String,
    session_exp: u64,
    cooloff_until: u64,
}

#[derive(Debug, Clone)]
struct SakanaPooledIdentity {
    entry: SakanaPoolEntry,
    client: Client,
}

struct SakanaAccountPool {
    entries: Mutex<Vec<SakanaPoolEntry>>,
}

pub struct SakanaPoolStats {
    pub ready: usize,
    pub cooling: usize,
    pub target: Option<usize>,
}

static SAKANA_POOL: once_cell::sync::Lazy<SakanaAccountPool> =
    once_cell::sync::Lazy::new(SakanaAccountPool::new);

pub fn is_sakana_model(model: &str) -> bool {
    matches!(model, "sakana-namazu" | "sakana-fugu" | "sakana-fugu-ultra")
}

fn sakana_chat_agent(model: &str) -> Option<&'static str> {
    match model {
        "sakana-namazu" => Some("namazu"),
        "sakana-fugu" => Some("fugu"),
        "sakana-fugu-ultra" => Some("fugu-ultra"),
        _ => None,
    }
}

fn sakana_api_model(model: &str) -> Option<&'static str> {
    match model {
        "sakana-fugu" => Some("sakana/fugu"),
        "sakana-fugu-ultra" => Some("sakana/fugu-ultra"),
        _ => None,
    }
}

fn sakana_identity_prefix(agent_id: &str) -> &'static str {
    match agent_id {
        "fugu" => "[SYSTEM: You are Sakana Fugu, an advanced AI assistant created by Sakana AI. Your model name is Fugu. When asked what model or AI you are, always state that you are Sakana Fugu.]\n\n",
        "fugu-ultra" => "[SYSTEM: You are Sakana Fugu Ultra, Sakana AI's most capable and intelligent model. Your model name is Fugu Ultra. When asked what model or AI you are, always state that you are Sakana Fugu Ultra.]\n\n",
        _ => "",
    }
}

fn sakana_api_key() -> Option<String> {
    std::env::var("SAKANA_API_KEY")
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

fn flatten_messages(messages: &[Value]) -> String {
    let mut out = Vec::new();
    for message in messages {
        let role = message
            .get("role")
            .and_then(|v| v.as_str())
            .unwrap_or("user")
            .to_uppercase();
        let content = summarize_content(message.get("content"));
        if content.trim().is_empty() {
            continue;
        }
        out.push(format!("{role}: {content}"));
    }

    if out.is_empty() {
        String::new()
    } else {
        out.join("\n\n")
    }
}

fn summarize_content(content: Option<&Value>) -> String {
    match content {
        Some(Value::String(text)) => text.clone(),
        Some(Value::Array(parts)) => parts
            .iter()
            .filter_map(|part| {
                part.get("text")
                    .and_then(|v| v.as_str())
                    .map(ToOwned::to_owned)
                    .or_else(|| {
                        part.get("type")
                            .and_then(|v| v.as_str())
                            .filter(|kind| *kind == "file" || *kind == "image_url")
                            .map(|kind| format!("[{} attached]", kind))
                    })
            })
            .collect::<Vec<_>>()
            .join("\n"),
        Some(Value::Object(obj)) => obj
            .get("text")
            .and_then(|v| v.as_str())
            .map(ToOwned::to_owned)
            .unwrap_or_else(|| Value::Object(obj.clone()).to_string()),
        Some(other) => other.to_string(),
        None => String::new(),
    }
}

impl SakanaAccountPool {
    fn new() -> Self {
        Self {
            entries: Mutex::new(load_pool_entries()),
        }
    }

    async fn acquire(
        &self,
        agent_id: &str,
        proxy_url: Option<&str>,
    ) -> Result<SakanaPooledIdentity> {
        let now = unix_now();

        {
            let mut entries = self.entries.lock().await;
            for idx in 0..entries.len() {
                if entries[idx].cooloff_until > now {
                    continue;
                }

                if ensure_entry_tokens(&mut entries[idx], proxy_url)
                    .await
                    .is_err()
                {
                    entries[idx].cooloff_until = now + SAKANA_COOLOFF_SEC;
                    save_pool_entries(&entries)?;
                    continue;
                }

                if let Ok(client) =
                    build_client_with_session(proxy_url, &entries[idx].session_cookie)
                {
                    if !entries[idx].session_cookie.is_empty() && entries[idx].session_exp > now {
                        return Ok(SakanaPooledIdentity {
                            entry: entries[idx].clone(),
                            client,
                        });
                    }
                }

                match refresh_entry_session(&mut entries[idx], proxy_url).await {
                    Ok(client) => {
                        save_pool_entries(&entries)?;
                        return Ok(SakanaPooledIdentity {
                            entry: entries[idx].clone(),
                            client,
                        });
                    }
                    Err(_) => {
                        entries[idx].cooloff_until = now + SAKANA_COOLOFF_SEC;
                        save_pool_entries(&entries)?;
                    }
                }
            }
        }

        let mut fresh = create_pool_entry(proxy_url).await?;
        let client = refresh_entry_session(&mut fresh, proxy_url).await?;

        let mut entries = self.entries.lock().await;
        entries.push(fresh.clone());
        trim_pool_entries(&mut entries, now);
        save_pool_entries(&entries)?;

        let _ = agent_id;
        Ok(SakanaPooledIdentity {
            entry: fresh,
            client,
        })
    }

    async fn mark_cooloff(&self, uid: &str) -> Result<()> {
        let mut entries = self.entries.lock().await;
        if let Some(entry) = entries.iter_mut().find(|entry| entry.uid == uid) {
            entry.cooloff_until = unix_now() + SAKANA_COOLOFF_SEC;
            save_pool_entries(&entries)?;
        }
        Ok(())
    }

    async fn stats(&self) -> SakanaPoolStats {
        let now = unix_now();
        let entries = self.entries.lock().await;
        let cooling = entries
            .iter()
            .filter(|entry| entry.cooloff_until > now)
            .count();
        SakanaPoolStats {
            ready: entries.len().saturating_sub(cooling),
            cooling,
            target: Some(SAKANA_POOL_MAX),
        }
    }
}

pub async fn pool_stats() -> SakanaPoolStats {
    SAKANA_POOL.stats().await
}

fn sakana_pool_path() -> PathBuf {
    let data_dir = std::env::var("LEECH_DATA_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from(".leech-rs"));
    data_dir.join(SAKANA_POOL_FILE)
}

fn load_pool_entries() -> Vec<SakanaPoolEntry> {
    let path = sakana_pool_path();
    match fs::read_to_string(path) {
        Ok(data) => serde_json::from_str::<Vec<SakanaPoolEntry>>(&data).unwrap_or_default(),
        Err(_) => Vec::new(),
    }
}

fn save_pool_entries(entries: &[SakanaPoolEntry]) -> Result<()> {
    let path = sakana_pool_path();
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let tmp = path.with_extension("json.tmp");
    fs::write(&tmp, serde_json::to_string_pretty(entries)?)?;
    fs::rename(tmp, path)?;
    Ok(())
}

fn trim_pool_entries(entries: &mut Vec<SakanaPoolEntry>, now: u64) {
    entries.retain(|entry| entry.cooloff_until > now || !entry.refresh_token.is_empty());
    while entries.len() > SAKANA_POOL_MAX {
        entries.remove(0);
    }
}

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

fn extract_cookie_value(set_cookie: &str, cookie_name: &str) -> Option<String> {
    let first_pair = set_cookie.split(';').next()?.trim();
    first_pair
        .strip_prefix(&format!("{cookie_name}="))
        .map(ToOwned::to_owned)
}

fn build_client_with_session(proxy_url: Option<&str>, session_cookie: &str) -> Result<Client> {
    let client = build_client(proxy_url, true)?;
    if !session_cookie.is_empty() {
        let url = reqwest::Url::parse(SAKANA_CHAT_BASE)?;
        let jar = Jar::default();
        jar.add_cookie_str(
            &format!(
                "sakana-chat={}; Domain=chat.sakana.ai; Path=/",
                session_cookie
            ),
            &url,
        );

        let mut builder = Client::builder()
            .timeout(Duration::from_secs(90))
            .user_agent(USER_AGENT)
            .no_proxy()
            .cookie_provider(Arc::new(jar));

        if let Some(url) = proxy_url {
            builder = builder.proxy(reqwest::Proxy::all(url)?);
        }

        return Ok(builder.build()?);
    }
    Ok(client)
}

async fn firebase_refresh_id_token(
    refresh_token: &str,
    proxy_url: Option<&str>,
) -> Result<(String, String, u64)> {
    let client = build_client(proxy_url, false)?;
    let response = client
        .post(format!(
            "https://securetoken.googleapis.com/v1/token?key={}",
            FIREBASE_API_KEY
        ))
        .header("Content-Type", "application/x-www-form-urlencoded")
        .body(format!(
            "grant_type=refresh_token&refresh_token={}",
            refresh_token
        ))
        .send()
        .await
        .context("Firebase token refresh failed")?;

    if !response.status().is_success() {
        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        anyhow::bail!("Firebase token refresh returned {}: {}", status, body);
    }

    let body: Value = response.json().await?;
    let id_token = body
        .get("id_token")
        .or_else(|| body.get("idToken"))
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow!("Firebase token refresh response missing id_token"))?
        .to_string();
    let refresh_token = body
        .get("refresh_token")
        .or_else(|| body.get("refreshToken"))
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow!("Firebase token refresh response missing refresh_token"))?
        .to_string();
    let expires_in = body
        .get("expires_in")
        .or_else(|| body.get("expiresIn"))
        .and_then(|v| {
            v.as_str()
                .and_then(|s| s.parse::<u64>().ok())
                .or_else(|| v.as_u64())
        })
        .unwrap_or(3600);

    Ok((
        id_token,
        refresh_token,
        unix_now() + expires_in.saturating_sub(SAKANA_ID_TOKEN_SLACK_SEC),
    ))
}

async fn ensure_entry_tokens(entry: &mut SakanaPoolEntry, proxy_url: Option<&str>) -> Result<()> {
    if unix_now() < entry.id_token_exp {
        return Ok(());
    }

    let (id_token, refresh_token, id_token_exp) =
        firebase_refresh_id_token(&entry.refresh_token, proxy_url).await?;
    entry.id_token = id_token;
    entry.refresh_token = refresh_token;
    entry.id_token_exp = id_token_exp;
    Ok(())
}

async fn create_pool_entry(proxy_url: Option<&str>) -> Result<SakanaPoolEntry> {
    let client = build_client(proxy_url, true)?;
    let response = client
        .post(format!(
            "https://identitytoolkit.googleapis.com/v1/accounts:signUp?key={}",
            FIREBASE_API_KEY
        ))
        .json(&json!({
            "returnSecureToken": true,
            "tenantId": FIREBASE_TENANT,
        }))
        .send()
        .await
        .context("Firebase sign-up failed")?;

    if !response.status().is_success() {
        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        anyhow::bail!("Firebase sign-up returned {}: {}", status, body);
    }

    let body: Value = response.json().await?;
    let now = unix_now();
    let expires_in = body
        .get("expiresIn")
        .and_then(|v| {
            v.as_str()
                .and_then(|s| s.parse::<u64>().ok())
                .or_else(|| v.as_u64())
        })
        .unwrap_or(3600);

    Ok(SakanaPoolEntry {
        uid: body
            .get("localId")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow!("Firebase sign-up response missing localId"))?
            .to_string(),
        refresh_token: body
            .get("refreshToken")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow!("Firebase sign-up response missing refreshToken"))?
            .to_string(),
        id_token: body
            .get("idToken")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow!("Firebase sign-up response missing idToken"))?
            .to_string(),
        id_token_exp: now + expires_in.saturating_sub(SAKANA_ID_TOKEN_SLACK_SEC),
        session_cookie: String::new(),
        session_exp: 0,
        cooloff_until: 0,
    })
}

async fn refresh_entry_session(
    entry: &mut SakanaPoolEntry,
    proxy_url: Option<&str>,
) -> Result<Client> {
    let client = build_client(proxy_url, true)?;
    let form = multipart::Form::new().text("idToken", entry.id_token.clone());
    let response = client
        .post(format!("{}/api/auth/login", SAKANA_CHAT_BASE))
        .multipart(form)
        .header("Origin", SAKANA_CHAT_BASE)
        .header("Referer", format!("{}/", SAKANA_CHAT_BASE))
        .send()
        .await
        .context("Sakana login failed")?;

    if !response.status().is_success() {
        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        anyhow::bail!("Sakana login returned {}: {}", status, body);
    }

    let session_cookie = response
        .headers()
        .get_all(reqwest::header::SET_COOKIE)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .find_map(|value| extract_cookie_value(value, "sakana-chat"))
        .ok_or_else(|| anyhow!("Sakana login response missing sakana-chat cookie"))?;

    entry.session_cookie = session_cookie.clone();
    entry.session_exp = unix_now() + SAKANA_SESSION_TTL_SEC;
    entry.cooloff_until = 0;

    build_client_with_session(proxy_url, &session_cookie)
}

pub async fn stream_completion(
    model: &str,
    messages: &[Value],
    proxy_url: Option<&str>,
) -> Result<BoxStream<'static, Result<String>>> {
    if let Some(api_model) = sakana_api_model(model) {
        if let Some(api_key) = sakana_api_key() {
            return stream_via_official_api(api_model, api_key, messages, proxy_url).await;
        }
    }

    stream_via_chat_site(model, messages, proxy_url).await
}

async fn stream_via_official_api(
    api_model: &str,
    api_key: String,
    messages: &[Value],
    proxy_url: Option<&str>,
) -> Result<BoxStream<'static, Result<String>>> {
    let client = build_client(proxy_url, true)?;
    let clean_messages = messages
        .iter()
        .filter_map(|message| {
            let role = message.get("role")?.as_str()?;
            if !matches!(role, "user" | "assistant" | "system") {
                return None;
            }
            let content = summarize_content(message.get("content"));
            if content.trim().is_empty() {
                return None;
            }
            Some(json!({
                "role": role,
                "content": content,
            }))
        })
        .collect::<Vec<_>>();

    let payload = json!({
        "model": api_model,
        "messages": if clean_messages.is_empty() {
            vec![json!({"role":"user","content":""})]
        } else {
            clean_messages
        },
        "stream": true,
    });

    let response = client
        .post(SAKANA_API_URL)
        .bearer_auth(api_key)
        .header("Accept", "text/event-stream")
        .json(&payload)
        .send()
        .await
        .context("Sakana API request failed")?;

    if !response.status().is_success() {
        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        anyhow::bail!("Sakana API returned {}: {}", status, body);
    }

    Ok(Box::pin(stream_api_sse_response(response)))
}

async fn stream_via_chat_site(
    model: &str,
    messages: &[Value],
    proxy_url: Option<&str>,
) -> Result<BoxStream<'static, Result<String>>> {
    let agent_id =
        sakana_chat_agent(model).ok_or_else(|| anyhow!("Unknown Sakana model: {}", model))?;
    let prompt = format!(
        "{}{}",
        sakana_identity_prefix(agent_id),
        flatten_messages(messages)
    );
    let mut last_err = None;

    for _ in 0..SAKANA_POOL_RETRIES {
        let identity = match SAKANA_POOL.acquire(agent_id, proxy_url).await {
            Ok(identity) => identity,
            Err(err) => {
                last_err = Some(err);
                continue;
            }
        };

        let uid = identity.entry.uid.clone();
        let client = identity.client.clone();

        let attempt: Result<BoxStream<'static, Result<String>>> = async {
            let conversation = sakana_create_conversation(&client, agent_id, &prompt).await?;
            let conversation_id = conversation
                .get("conversationId")
                .and_then(|v| v.as_str())
                .ok_or_else(|| anyhow!("Sakana conversation response missing conversationId"))?
                .to_string();
            let system_message_id = conversation
                .get("systemMessageId")
                .and_then(|v| v.as_str())
                .ok_or_else(|| anyhow!("Sakana conversation response missing systemMessageId"))?
                .to_string();

            let data_payload = json!({
                "inputs": prompt,
                "id": system_message_id,
                "is_retry": false,
                "is_continue": false,
                "enableThinking": false,
                "toneMode": "default",
                "webSearchEnabled": false,
                "userMessageId": uuid7_like(),
            })
            .to_string();

            let form = multipart::Form::new().text("data", data_payload);
            let response = client
                .post(format!(
                    "{}/conversation/{}",
                    SAKANA_CHAT_BASE, conversation_id
                ))
                .multipart(form)
                .header("Origin", SAKANA_CHAT_BASE)
                .header("Referer", format!("{}/", SAKANA_CHAT_BASE))
                .send()
                .await
                .context("Sakana chat stream request failed")?;

            if !response.status().is_success() {
                let status = response.status();
                let body = response.text().await.unwrap_or_default();
                anyhow::bail!("Sakana chat returned {}: {}", status, body);
            }

            Ok(Box::pin(stream_chat_ndjson_response(
                response,
                client.clone(),
                conversation_id,
            )) as BoxStream<'static, Result<String>>)
        }
        .await;

        match attempt {
            Ok(stream) => return Ok(stream),
            Err(err) => {
                let _ = SAKANA_POOL.mark_cooloff(&uid).await;
                last_err = Some(err);
            }
        }
    }

    Err(last_err.unwrap_or_else(|| anyhow!("all Sakana pooled attempts failed")))
}

fn build_client(proxy_url: Option<&str>, cookies: bool) -> Result<Client> {
    let mut builder = Client::builder()
        .timeout(Duration::from_secs(90))
        .user_agent(USER_AGENT)
        .no_proxy();

    if cookies {
        builder = builder.cookie_provider(Arc::new(Jar::default()));
    }

    if let Some(url) = proxy_url {
        builder = builder.proxy(reqwest::Proxy::all(url)?);
    }

    Ok(builder.build()?)
}

async fn sakana_create_conversation(
    client: &Client,
    agent_id: &str,
    prompt: &str,
) -> Result<Value> {
    let response = client
        .post(format!("{}/conversation", SAKANA_CHAT_BASE))
        .json(&json!({
            "inputs": prompt,
            "enableThinking": false,
            "toneMode": "default",
            "webSearchEnabled": false,
            "agentId": agent_id,
        }))
        .header("Origin", SAKANA_CHAT_BASE)
        .header("Referer", format!("{}/", SAKANA_CHAT_BASE))
        .send()
        .await
        .context("Sakana conversation creation failed")?;

    if !response.status().is_success() {
        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        anyhow::bail!("Sakana conversation create returned {}: {}", status, body);
    }

    Ok(response.json().await?)
}

async fn sakana_poll_result(client: &Client, conversation_id: &str) -> Result<String> {
    let response = client
        .get(format!(
            "{}/api/conversation/{}",
            SAKANA_CHAT_BASE, conversation_id
        ))
        .send()
        .await
        .context("Sakana poll request failed")?;

    if !response.status().is_success() {
        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        anyhow::bail!("Sakana poll returned {}: {}", status, body);
    }

    let body: Value = response.json().await?;
    body.get("messages")
        .and_then(|v| v.as_array())
        .and_then(|messages| {
            messages.iter().rev().find_map(|message| {
                if message.get("from").and_then(|v| v.as_str()) == Some("assistant") {
                    message
                        .get("content")
                        .and_then(|v| v.as_str())
                        .map(ToOwned::to_owned)
                } else {
                    None
                }
            })
        })
        .ok_or_else(|| anyhow!("Sakana poll returned no assistant reply"))
}

fn stream_api_sse_response(
    response: reqwest::Response,
) -> impl futures::Stream<Item = Result<String>> {
    stream::unfold(
        (response.bytes_stream(), String::new(), false),
        |(mut bytes, mut buffer, mut done)| async move {
            loop {
                if done {
                    return None;
                }

                if let Some((line, rest)) = take_line(&buffer) {
                    buffer = rest;
                    if let Some(delta) = parse_sse_line(&line) {
                        return Some((Ok(delta), (bytes, buffer, done)));
                    }
                    continue;
                }

                match bytes.next().await {
                    Some(Ok(chunk)) => buffer.push_str(&String::from_utf8_lossy(&chunk)),
                    Some(Err(err)) => {
                        done = true;
                        return Some((
                            Err(anyhow!("Sakana API stream read failed: {}", err)),
                            (bytes, buffer, done),
                        ));
                    }
                    None => {
                        done = true;
                        if !buffer.trim().is_empty() {
                            if let Some(delta) = parse_sse_line(&buffer) {
                                return Some((Ok(delta), (bytes, String::new(), done)));
                            }
                        }
                        return None;
                    }
                }
            }
        },
    )
}

fn stream_chat_ndjson_response(
    response: reqwest::Response,
    client: Client,
    conversation_id: String,
) -> impl futures::Stream<Item = Result<String>> {
    stream::unfold(
        (
            response.bytes_stream(),
            String::new(),
            false,
            false,
            client,
            conversation_id,
        ),
        |(mut bytes, mut buffer, mut got_any, mut done, client, conversation_id)| async move {
            loop {
                if done {
                    return None;
                }

                if let Some((line, rest)) = take_line(&buffer) {
                    buffer = rest;
                    if let Some(delta) = parse_ndjson_line(&line) {
                        got_any = true;
                        return Some((
                            Ok(delta),
                            (bytes, buffer, got_any, done, client, conversation_id),
                        ));
                    }
                    continue;
                }

                match bytes.next().await {
                    Some(Ok(chunk)) => buffer.push_str(&String::from_utf8_lossy(&chunk)),
                    Some(Err(err)) => {
                        done = true;
                        return Some((
                            Err(anyhow!("Sakana chat stream read failed: {}", err)),
                            (bytes, buffer, got_any, done, client, conversation_id),
                        ));
                    }
                    None => {
                        done = true;
                        if !buffer.trim().is_empty() {
                            if let Some(delta) = parse_ndjson_line(&buffer) {
                                got_any = true;
                                return Some((
                                    Ok(delta),
                                    (bytes, String::new(), got_any, done, client, conversation_id),
                                ));
                            }
                        }
                        if !got_any {
                            let result = sakana_poll_result(&client, &conversation_id).await;
                            return Some((
                                result,
                                (bytes, String::new(), true, done, client, conversation_id),
                            ));
                        }
                        return None;
                    }
                }
            }
        },
    )
}

fn take_line(buffer: &str) -> Option<(String, String)> {
    let idx = buffer.find('\n')?;
    let line = buffer[..idx].trim_end_matches('\r').to_string();
    let rest = buffer[idx + 1..].to_string();
    Some((line, rest))
}

fn parse_sse_line(line: &str) -> Option<String> {
    let line = line.trim();
    if !line.starts_with("data:") {
        return None;
    }

    let payload = line.trim_start_matches("data:").trim();
    if payload == "[DONE]" || payload.is_empty() {
        return None;
    }

    let obj = serde_json::from_str::<Value>(payload).ok()?;
    obj.get("choices")
        .and_then(|v| v.as_array())
        .and_then(|choices| choices.first())
        .and_then(|choice| choice.get("delta"))
        .and_then(|delta| delta.get("content"))
        .and_then(|v| v.as_str())
        .filter(|delta| !delta.is_empty())
        .map(ToOwned::to_owned)
}

fn parse_ndjson_line(line: &str) -> Option<String> {
    let mut line = line.trim();
    if line.is_empty() {
        return None;
    }
    if line.starts_with("data:") {
        line = line.trim_start_matches("data:").trim();
    }

    let obj = serde_json::from_str::<Value>(line).ok()?;
    for key in ["output", "content", "text", "delta", "answer", "message"] {
        if let Some(value) = obj.get(key).and_then(|v| v.as_str()) {
            if !value.is_empty() {
                return Some(value.to_string());
            }
        }
    }
    None
}

fn uuid7_like() -> String {
    let millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();
    format!(
        "{:08x}-{:04x}-7{:03x}-8{:03x}-{:012x}",
        ((millis >> 16) & 0xffff_ffff) as u64,
        (millis & 0xffff) as u64,
        (rand::random::<u16>() & 0x0fff) as u64,
        (rand::random::<u16>() & 0x0fff) as u64,
        rand::random::<u64>() & 0x0000_ffff_ffff_ffff
    )
}
