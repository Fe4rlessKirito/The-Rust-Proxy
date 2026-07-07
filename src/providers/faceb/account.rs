use anyhow::{anyhow, Context, Result};
use rand::seq::SliceRandom;
use rand::Rng;
use regex::Regex;
use reqwest::Client;
use scraper::{Html, Selector};
use std::time::Duration;

use super::FACEB_BASE;

const USER_AGENTS: &[&str] = &[
    "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/131.0.0.0 Safari/537.36",
    "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/131.0.0.0 Safari/537.36",
    "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/131.0.0.0 Safari/537.36",
];

fn auth_failure_context(response: &reqwest::Response) -> Option<String> {
    let path = response.url().path().trim_end_matches('/');
    if matches!(path, "/signup" | "/login") {
        Some(format!("redirected to {}", response.url().path()))
    } else {
        None
    }
}

fn body_looks_like_auth_page(body: &str) -> bool {
    let lower = body.to_lowercase();
    lower.contains("csrfmiddlewaretoken")
        && (lower.contains("sign up") || lower.contains("log in") || lower.contains("login"))
}

pub(super) async fn make_account(proxy_url: Option<&str>) -> Result<Option<String>> {
    let user_agent = USER_AGENTS
        .choose(&mut rand::thread_rng())
        .copied()
        .unwrap_or(USER_AGENTS[0]);
    let client = build_client(proxy_url, Duration::from_secs(30))?;
    let signup = client
        .get(format!("{}/signup/", FACEB_BASE))
        .header("User-Agent", user_agent)
        .send()
        .await
        .context("Faceb signup page request failed")?;
    if !signup.status().is_success() {
        anyhow::bail!("Faceb signup page returned HTTP {}", signup.status());
    }
    let signup_body = signup.text().await?;
    let Some(csrf) = extract_csrf(&signup_body) else {
        anyhow::bail!("Faceb signup page CSRF token not found");
    };
    let email = random_email();
    let password = random_password();
    let signup_response = client
        .post(format!("{}/signup/", FACEB_BASE))
        .header("User-Agent", user_agent)
        .header("Referer", format!("{}/signup/", FACEB_BASE))
        .form(&[
            ("csrfmiddlewaretoken", csrf.as_str()),
            ("email", email.as_str()),
            ("password", password.as_str()),
        ])
        .send()
        .await
        .context("Faceb signup submit failed")?;
    if !signup_response.status().is_success() {
        anyhow::bail!(
            "Faceb signup submit returned HTTP {}",
            signup_response.status()
        );
    }
    if let Some(context) = auth_failure_context(&signup_response) {
        anyhow::bail!("Faceb signup validation failed: {}", context);
    }
    let signup_response_body = signup_response.text().await.unwrap_or_default();
    if body_looks_like_auth_page(&signup_response_body) {
        anyhow::bail!("Faceb signup validation failed: auth page rendered after submit");
    }
    let api_page = client
        .get(format!("{}/account/api/", FACEB_BASE))
        .header("User-Agent", user_agent)
        .send()
        .await
        .context("Faceb API page request failed")?;
    if !api_page.status().is_success() {
        anyhow::bail!("Faceb API page returned HTTP {}", api_page.status());
    }
    if let Some(context) = auth_failure_context(&api_page) {
        anyhow::bail!("Faceb API page auth failed: {}", context);
    }
    let api_page_body = api_page.text().await?;
    if body_looks_like_auth_page(&api_page_body) {
        anyhow::bail!("Faceb API page rendered auth page");
    }
    let Some(csrf) = extract_csrf(&api_page_body) else {
        anyhow::bail!("Faceb API page CSRF token not found");
    };
    let key_page = client
        .post(format!("{}/account/api/new/", FACEB_BASE))
        .header("User-Agent", user_agent)
        .header("Referer", format!("{}/account/api/", FACEB_BASE))
        .form(&[("csrfmiddlewaretoken", csrf.as_str()), ("name", "")])
        .send()
        .await
        .context("Faceb API key create failed")?;
    if !key_page.status().is_success() {
        anyhow::bail!("Faceb API key create returned HTTP {}", key_page.status());
    }
    if let Some(context) = auth_failure_context(&key_page) {
        anyhow::bail!("Faceb API key create auth failed: {}", context);
    }
    let key_body = key_page.text().await.unwrap_or_default();
    extract_key(&key_body)
        .ok_or_else(|| anyhow!("Faceb API key not found in response"))
        .map(Some)
}

pub(super) fn build_client(proxy_url: Option<&str>, timeout: Duration) -> Result<Client> {
    let mut builder = Client::builder()
        .timeout(timeout)
        .cookie_store(true)
        .user_agent(USER_AGENTS[0])
        .no_proxy();
    if let Some(url) = proxy_url {
        builder = builder.proxy(reqwest::Proxy::all(url)?);
    }
    Ok(builder.build()?)
}

fn extract_csrf(html: &str) -> Option<String> {
    let document = Html::parse_document(html);
    let selector = Selector::parse(r#"input[name="csrfmiddlewaretoken"]"#).ok()?;
    document
        .select(&selector)
        .find_map(|element| element.value().attr("value"))
        .map(ToOwned::to_owned)
}

fn extract_key(text: &str) -> Option<String> {
    static KEY_RE: once_cell::sync::Lazy<Regex> = once_cell::sync::Lazy::new(|| {
        Regex::new(r"sk-faceb-[a-zA-Z0-9_-]+").expect("valid Faceb key regex")
    });
    KEY_RE.find(text).map(|m| m.as_str().to_string())
}

fn random_email() -> String {
    format!("{}@gmail.com", random_alnum(10).to_lowercase())
}

fn random_password() -> String {
    let mut rng = rand::thread_rng();
    let symbols = ['!', '@', '#', '$', '%'];
    format!(
        "{}{}{}{}{}",
        rng.gen_range(b'A'..=b'Z') as char,
        rng.gen_range(b'a'..=b'z') as char,
        rng.gen_range(b'0'..=b'9') as char,
        *symbols.choose(&mut rng).unwrap_or(&'!'),
        random_alnum(8)
    )
}

fn random_alnum(len: usize) -> String {
    const CHARS: &[u8] = b"abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789";
    let mut rng = rand::thread_rng();
    (0..len)
        .map(|_| CHARS[rng.gen_range(0..CHARS.len())] as char)
        .collect()
}
