#![allow(dead_code)]
//! Token / message usage metering, persisted to ~/.leech-rs/usage.json.
//!
//! The upstream provider does not return token counts (it's a leeched stream), so we
//! estimate tokens from text length using a ~4-chars-per-token heuristic. That is
//! accurate enough for spend awareness and per-conversation caps.
//!
//! Data model (usage.json):
//! {
//!   "sessions": {
//!      "<session_id>": {
//!         "messages": int,           // user+assistant turns counted
//!         "input_tokens": int,
//!         "output_tokens": int,
//!         "first_ts": f64,
//!         "last_ts": f64,
//!         "models": {"<model>": int, ...},   // output-token count per model
//!         "cap": Option<u64>         // per-conversation token cap (None = no cap)
//!      }, ...
//!   },
//!   "daily": {"YYYY-MM-DD": u64, ...},        // total tokens per calendar day
//!   "hourly": {"0".. "23": u64, ...},          // total tokens per local hour
//!   "models": {"<model>": u64, ...}            // global output-token count per model
//! }

use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

const CHARS_PER_TOKEN: f64 = 4.0;
const DATA_DIR: &str = ".leech-rs";
const STORE_PATH: &str = "usage.json";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionStats {
    pub messages: u64,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub first_ts: f64,
    pub last_ts: f64,
    pub models: HashMap<String, u64>,
    pub cap: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UsageData {
    pub sessions: HashMap<String, SessionStats>,
    pub daily: HashMap<String, u64>,
    pub hourly: HashMap<String, u64>,
    pub models: HashMap<String, u64>,
    #[serde(default)]
    pub model_input_tokens: HashMap<String, u64>,
    #[serde(default)]
    pub model_output_tokens: HashMap<String, u64>,
}

#[derive(Debug, Clone, Serialize)]
pub struct SessionSnapshot {
    pub session_id: String,
    pub messages: u64,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub total_tokens: u64,
    pub cap: Option<u64>,
    pub cap_reached: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct Overview {
    pub sessions: usize,
    pub messages: u64,
    pub total_tokens: u64,
    pub active_days: usize,
    pub current_streak: u64,
    pub longest_streak: u64,
    pub peak_hour: Option<String>,
    pub favorite_model: Option<String>,
    pub daily: HashMap<String, u64>,
    pub models: HashMap<String, u64>,
    pub model_input_tokens: HashMap<String, u64>,
    pub model_output_tokens: HashMap<String, u64>,
}

lazy_static::lazy_static! {
    static ref DATA: Mutex<UsageData> = Mutex::new(UsageData {
        sessions: HashMap::new(),
        daily: HashMap::new(),
        hourly: HashMap::new(),
        models: HashMap::new(),
        model_input_tokens: HashMap::new(),
        model_output_tokens: HashMap::new(),
    });
}

fn empty_session(now: f64) -> SessionStats {
    SessionStats {
        messages: 0,
        input_tokens: 0,
        output_tokens: 0,
        first_ts: now,
        last_ts: now,
        models: HashMap::new(),
        cap: None,
    }
}

pub fn estimate_tokens(text: &str) -> u64 {
    if text.is_empty() {
        return 0;
    }
    (text.len() as f64 / CHARS_PER_TOKEN).round().max(1.0) as u64
}

pub fn estimate_messages_tokens(messages: &[serde_json::Value]) -> u64 {
    let mut total = 0;
    for m in messages {
        let content = m.get("content");
        match content {
            Some(serde_json::Value::String(s)) => total += s.len(),
            Some(serde_json::Value::Array(blocks)) => {
                for b in blocks {
                    if let Some(text) = b.get("text").and_then(|v| v.as_str()) {
                        total += text.len();
                    }
                }
            }
            _ => {}
        }
    }
    (total as f64 / CHARS_PER_TOKEN).round().max(0.0) as u64
}

fn load() -> Result<()> {
    let path = usage_data_dir().join(STORE_PATH);
    if !path.exists() {
        return Ok(());
    }
    let data = fs::read_to_string(&path)?;
    let loaded: UsageData = serde_json::from_str(&data)?;
    *DATA.lock().unwrap() = loaded;
    Ok(())
}

fn save() -> Result<()> {
    let dir = usage_data_dir();
    fs::create_dir_all(&dir)?;
    let tmp = dir.join(STORE_PATH.to_owned() + ".tmp");
    let snapshot = DATA.lock().unwrap().clone();
    let data = serde_json::to_string_pretty(&snapshot)?;
    fs::write(&tmp, data)?;
    fs::rename(tmp, dir.join(STORE_PATH))?;
    Ok(())
}

fn now_secs() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs_f64()
}

fn session(sid: &str) -> SessionStats {
    let mut data = DATA.lock().unwrap();
    if let Some(s) = data.sessions.get(sid) {
        return s.clone();
    }
    let now = now_secs();
    let s = empty_session(now);
    data.sessions.insert(sid.to_string(), s.clone());
    s
}

pub fn record(
    session_id: &str,
    model: &str,
    input_text: &str,
    output_text: &str,
) -> Result<SessionSnapshot> {
    let sid = if session_id.is_empty() {
        "default"
    } else {
        session_id
    };
    let in_tok = estimate_tokens(input_text);
    let out_tok = estimate_tokens(output_text);
    let now = now_secs();
    let day = chrono::Local::now().format("%Y-%m-%d").to_string();
    let hour = chrono::Local::now().format("%H").to_string();

    let mut data = DATA.lock().unwrap();
    data.sessions
        .entry(sid.to_string())
        .or_insert_with(|| empty_session(now));
    let s = data.sessions.get_mut(sid).unwrap();
    s.messages += 1;
    s.input_tokens += in_tok;
    s.output_tokens += out_tok;
    s.last_ts = now;
    let cap = s.cap;
    let messages = s.messages;
    let input_tokens = s.input_tokens;
    let output_tokens = s.output_tokens;
    if !model.is_empty() {
        *s.models.entry(model.to_string()).or_insert(0) += out_tok;
    }
    let total = in_tok + out_tok;
    let total_tokens = input_tokens + output_tokens;
    let cap_reached = cap.is_some_and(|c| total_tokens >= c);
    let _ = s;
    if !model.is_empty() {
        *data.models.entry(model.to_string()).or_insert(0) += out_tok;
        *data
            .model_input_tokens
            .entry(model.to_string())
            .or_insert(0) += in_tok;
        *data
            .model_output_tokens
            .entry(model.to_string())
            .or_insert(0) += out_tok;
    }
    *data.daily.entry(day).or_insert(0) += total;
    *data.hourly.entry(hour).or_insert(0) += total;
    let snap = SessionSnapshot {
        session_id: sid.to_string(),
        messages,
        input_tokens,
        output_tokens,
        total_tokens,
        cap,
        cap_reached,
    };
    drop(data);
    save()?;
    Ok(snap)
}

pub fn record_tokens(
    session_id: &str,
    model: &str,
    in_tok: u64,
    out_tok: u64,
) -> Result<SessionSnapshot> {
    let sid = if session_id.is_empty() {
        "default"
    } else {
        session_id
    };
    let now = now_secs();
    let day = chrono::Local::now().format("%Y-%m-%d").to_string();
    let hour = chrono::Local::now().format("%H").to_string();

    let mut data = DATA.lock().unwrap();
    data.sessions
        .entry(sid.to_string())
        .or_insert_with(|| empty_session(now));
    let s = data.sessions.get_mut(sid).unwrap();
    s.messages += 1;
    s.input_tokens += in_tok;
    s.output_tokens += out_tok;
    s.last_ts = now;
    let cap = s.cap;
    let messages = s.messages;
    let input_tokens = s.input_tokens;
    let output_tokens = s.output_tokens;
    if !model.is_empty() {
        *s.models.entry(model.to_string()).or_insert(0) += out_tok;
    }
    let total = in_tok + out_tok;
    let total_tokens = input_tokens + output_tokens;
    let cap_reached = cap.is_some_and(|c| total_tokens >= c);
    let _ = s;
    if !model.is_empty() {
        *data.models.entry(model.to_string()).or_insert(0) += out_tok;
        *data
            .model_input_tokens
            .entry(model.to_string())
            .or_insert(0) += in_tok;
        *data
            .model_output_tokens
            .entry(model.to_string())
            .or_insert(0) += out_tok;
    }
    *data.daily.entry(day).or_insert(0) += total;
    *data.hourly.entry(hour).or_insert(0) += total;
    let snap = SessionSnapshot {
        session_id: sid.to_string(),
        messages,
        input_tokens,
        output_tokens,
        total_tokens,
        cap,
        cap_reached,
    };
    drop(data);
    save()?;
    Ok(snap)
}

pub fn set_cap(session_id: &str, cap: Option<u64>) -> Result<SessionSnapshot> {
    let sid = if session_id.is_empty() {
        "default"
    } else {
        session_id
    };
    let mut data = DATA.lock().unwrap();
    let now = now_secs();
    data.sessions
        .entry(sid.to_string())
        .or_insert_with(|| empty_session(now));
    let s = data.sessions.get_mut(sid).unwrap();
    s.cap = cap;
    let total = s.input_tokens + s.output_tokens;
    let snap = SessionSnapshot {
        session_id: sid.to_string(),
        messages: s.messages,
        input_tokens: s.input_tokens,
        output_tokens: s.output_tokens,
        total_tokens: total,
        cap,
        cap_reached: cap.is_some_and(|c| total >= c),
    };
    drop(data);
    save()?;
    Ok(snap)
}

pub fn session_snapshot(session_id: &str) -> SessionSnapshot {
    let sid = if session_id.is_empty() {
        "default"
    } else {
        session_id
    };
    let data = DATA.lock().unwrap();
    if let Some(s) = data.sessions.get(sid) {
        let total = s.input_tokens + s.output_tokens;
        SessionSnapshot {
            session_id: sid.to_string(),
            messages: s.messages,
            input_tokens: s.input_tokens,
            output_tokens: s.output_tokens,
            total_tokens: total,
            cap: s.cap,
            cap_reached: s.cap.is_some_and(|c| total >= c),
        }
    } else {
        SessionSnapshot {
            session_id: sid.to_string(),
            messages: 0,
            input_tokens: 0,
            output_tokens: 0,
            total_tokens: 0,
            cap: None,
            cap_reached: false,
        }
    }
}

pub fn cap_exceeded(session_id: &str) -> bool {
    session_snapshot(session_id).cap_reached
}

fn streak_days(daily: &HashMap<String, u64>) -> (u64, u64) {
    if daily.is_empty() {
        return (0, 0);
    }
    let days: Vec<_> = daily
        .iter()
        .filter(|(_, &v)| v > 0)
        .map(|(d, _)| d.clone())
        .collect();
    if days.is_empty() {
        return (0, 0);
    }
    let mut dates: Vec<_> = days
        .into_iter()
        .filter_map(|d| {
            let parts: Vec<_> = d.split('-').collect();
            if parts.len() != 3 {
                return None;
            }
            chrono::NaiveDate::from_ymd_opt(
                parts[0].parse().ok()?,
                parts[1].parse().ok()?,
                parts[2].parse().ok()?,
            )
        })
        .collect();
    if dates.is_empty() {
        return (0, 0);
    }
    dates.sort();
    let mut longest = 1;
    let mut run = 1;
    for i in 1..dates.len() {
        if (dates[i] - dates[i - 1]).num_days() == 1 {
            run += 1;
            longest = longest.max(run);
        } else {
            run = 1;
        }
    }
    let dset: std::collections::HashSet<_> = dates.into_iter().collect();
    let today = chrono::Local::now().date_naive();
    let mut current = 0;
    let mut cursor = today;
    if !dset.contains(&cursor) {
        cursor -= chrono::Duration::days(1);
    }
    while dset.contains(&cursor) {
        current += 1;
        cursor -= chrono::Duration::days(1);
    }
    (current, longest)
}

pub fn overview() -> Overview {
    let data = DATA.lock().unwrap();
    let sessions = &data.sessions;
    let daily = data.daily.clone();
    let hourly = data.hourly.clone();
    let models = data.models.clone();
    let model_input_tokens = data.model_input_tokens.clone();
    let model_output_tokens = data.model_output_tokens.clone();

    let total_sessions = sessions.len();
    let total_messages: u64 = sessions.values().map(|s| s.messages).sum();
    let total_tokens: u64 = sessions
        .values()
        .map(|s| s.input_tokens + s.output_tokens)
        .sum();
    let active_days = daily.values().filter(|&&v| v > 0).count();
    let (current_streak, longest_streak) = streak_days(&daily);

    let peak_hour = hourly
        .iter()
        .filter_map(|(h, &v)| h.parse::<u32>().ok().filter(|h| *h < 24).map(|h| (h, v)))
        .max_by_key(|(_, v)| *v)
        .map(|(h, _)| {
            let ampm = if h < 12 { "AM" } else { "PM" };
            let disp = h % 12;
            let disp = if disp == 0 { 12 } else { disp };
            format!("{} {}", disp, ampm)
        });

    let favorite_model = models
        .iter()
        .max_by_key(|(_, &v)| v)
        .map(|(m, _)| m.clone());

    Overview {
        sessions: total_sessions,
        messages: total_messages,
        total_tokens,
        active_days,
        current_streak,
        longest_streak,
        peak_hour,
        favorite_model,
        daily,
        models,
        model_input_tokens,
        model_output_tokens,
    }
}

pub fn reset_all() -> Result<()> {
    *DATA.lock().unwrap() = UsageData {
        sessions: HashMap::new(),
        daily: HashMap::new(),
        hourly: HashMap::new(),
        models: HashMap::new(),
        model_input_tokens: HashMap::new(),
        model_output_tokens: HashMap::new(),
    };
    save()
}

pub fn init() -> Result<()> {
    load()
}

fn usage_data_dir() -> PathBuf {
    std::env::var("LEECH_DATA_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from(DATA_DIR))
}
