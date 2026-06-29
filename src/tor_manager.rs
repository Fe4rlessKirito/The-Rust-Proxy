//! Dynamic Tor instance manager.

use anyhow::{anyhow, Result};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;
use tokio::net::TcpStream;
use tokio::process::{Child, Command};
use tokio::sync::{watch, Mutex};
use tokio::time::sleep;
use tracing::{info, warn};

pub struct TorManager {
    next_port: Arc<Mutex<u16>>,
    active: Arc<Mutex<HashMap<u16, Option<Child>>>>,
    proxy_list: Arc<Mutex<Vec<String>>>,
    proxy_sender: watch::Sender<Vec<String>>,
    proxy_receiver: watch::Receiver<Vec<String>>,
}

impl TorManager {
    pub fn new(base_port: u16) -> Self {
        let (tx, rx) = watch::channel(Vec::new());
        Self {
            next_port: Arc::new(Mutex::new(base_port)),
            active: Arc::new(Mutex::new(HashMap::new())),
            proxy_list: Arc::new(Mutex::new(Vec::new())),
            proxy_sender: tx,
            proxy_receiver: rx,
        }
    }

    pub async fn add_existing_or_spawn(&self, port: u16) -> Result<String> {
        {
            let mut next = self.next_port.lock().await;
            if *next <= port {
                *next = port.saturating_add(1);
            }
        }

        if tor_socks_reachable("127.0.0.1", port).await {
            let url = self.register_proxy(port, None).await;
            info!("Tor SOCKS proxy already running on port {}", port);
            return Ok(url);
        }

        self.spawn_proxy_on_port(port).await
    }

    pub async fn spawn_new_proxy(&self) -> Result<String> {
        loop {
            let port = {
                let mut next = self.next_port.lock().await;
                let port = *next;
                *next = next.saturating_add(1);
                port
            };

            if self.active.lock().await.contains_key(&port) {
                continue;
            }

            if tor_socks_reachable("127.0.0.1", port).await {
                return Ok(self.register_proxy(port, None).await);
            }

            return self.spawn_proxy_on_port(port).await;
        }
    }

    async fn spawn_proxy_on_port(&self, port: u16) -> Result<String> {
        let tor_exe = find_tor_exe()?;
        let data_dir = PathBuf::from(format!("tor_data_{}", port));
        if !data_dir.exists() {
            tokio::fs::create_dir_all(&data_dir).await?;
        }

        let control_port = port.saturating_add(10_000);
        let mut cmd = Command::new(&tor_exe);
        cmd.arg("--SocksPort")
            .arg(port.to_string())
            .arg("--ControlPort")
            .arg(control_port.to_string())
            .arg("--CookieAuthentication")
            .arg("1")
            .arg("--DataDirectory")
            .arg(data_dir.as_os_str())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .stdin(Stdio::null());

        #[cfg(windows)]
        cmd.creation_flags(0x08000000);

        let mut child = cmd.spawn().map_err(|e| anyhow!("Failed to spawn tor.exe: {}", e))?;
        let start = std::time::Instant::now();
        while start.elapsed() < Duration::from_secs(20) {
            if tor_socks_reachable("127.0.0.1", port).await {
                let url = self.register_proxy(port, Some(child)).await;
                info!("Spawned new Tor proxy on port {}", port);
                return Ok(url);
            }
            sleep(Duration::from_millis(200)).await;
        }

        let _ = child.kill().await;
        anyhow::bail!("Tor on port {} did not become ready within timeout", port)
    }

    async fn register_proxy(&self, port: u16, child: Option<Child>) -> String {
        let url = format!("socks5h://127.0.0.1:{}", port);
        let mut active = self.active.lock().await;
        active.entry(port).or_insert(child);
        drop(active);

        let mut list = self.proxy_list.lock().await;
        if !list.iter().any(|p| p == &url) {
            list.push(url.clone());
            let _ = self.proxy_sender.send(list.clone());
        }
        url
    }

    pub async fn get_proxies(&self) -> Vec<String> {
        self.proxy_list.lock().await.clone()
    }

    pub fn subscribe(&self) -> watch::Receiver<Vec<String>> {
        self.proxy_receiver.clone()
    }

    pub async fn remove_proxy(&self, port: u16) -> Result<()> {
        let mut active = self.active.lock().await;
        match active.remove(&port) {
            Some(Some(mut child)) => {
                let _ = child.kill().await;
                let _ = child.wait().await;
            }
            Some(None) => {
                warn!("Removed externally managed Tor proxy {} from rotation only", port);
            }
            None => return Err(anyhow!("No active Tor proxy on port {}", port)),
        }
        drop(active);

        let mut list = self.proxy_list.lock().await;
        list.retain(|url| !url.ends_with(&format!(":{}", port)));
        let _ = self.proxy_sender.send(list.clone());
        info!("Removed Tor proxy on port {}", port);
        Ok(())
    }

    pub async fn stop_all(&self) -> Result<()> {
        let mut active = self.active.lock().await;
        for (port, child) in active.drain() {
            if let Some(mut child) = child {
                let _ = child.kill().await;
                let _ = child.wait().await;
                info!("Stopped Tor proxy on port {}", port);
            }
        }
        drop(active);

        let mut list = self.proxy_list.lock().await;
        list.clear();
        let _ = self.proxy_sender.send(Vec::new());
        Ok(())
    }

    pub async fn count(&self) -> usize {
        self.proxy_list.lock().await.len()
    }
}

async fn tor_socks_reachable(host: &str, port: u16) -> bool {
    TcpStream::connect(format!("{}:{}", host, port)).await.is_ok()
}

fn find_tor_exe() -> Result<PathBuf> {
    let exe_path = std::env::current_exe()?;
    let exe_dir = exe_path.parent().unwrap_or(Path::new("."));
    let candidates = [
        exe_dir.join("tor").join("tor.exe"),
        exe_dir.join("tor.exe"),
        PathBuf::from("tor").join("tor.exe"),
        PathBuf::from("tor.exe"),
    ];

    for candidate in candidates {
        if candidate.exists() {
            return Ok(candidate);
        }
    }

    if let Ok(path) = which::which("tor.exe") {
        return Ok(path);
    }

    Err(anyhow!("tor.exe not found in ./tor/ or PATH"))
}
