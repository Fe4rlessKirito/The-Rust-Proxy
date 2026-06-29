//! Utility functions: email generation, time, etc.

use rand::Rng;
use std::time::{SystemTime, UNIX_EPOCH};

const LOCAL_MIN: usize = 8;
const LOCAL_MAX: usize = 14;
const DOMAIN_MIN: usize = 5;
const DOMAIN_MAX: usize = 9;
const TLDS: &[&str] = &["com", "net", "org", "io", "co", "xyz"];

pub fn gen_email() -> String {
    let mut rng = rand::thread_rng();
    let local_len = rng.gen_range(LOCAL_MIN..=LOCAL_MAX);
    let domain_len = rng.gen_range(DOMAIN_MIN..=DOMAIN_MAX);
    let local: String = (0..local_len)
        .map(|_| rng.sample(rand::distributions::Alphanumeric) as char)
        .collect();
    let domain: String = (0..domain_len)
        .map(|_| rng.sample(rand::distributions::Alphanumeric) as char)
        .collect();
    let tld = TLDS[rng.gen_range(0..TLDS.len())];
    format!(
        "{}.{}@{}.{}",
        local,
        uuid::Uuid::new_v4().simple().to_string()[..4].to_lowercase(),
        domain,
        tld
    )
}

pub fn now_secs() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs_f64()
}