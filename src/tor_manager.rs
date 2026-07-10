//! Dynamic Tor instance manager.

use anyhow::{anyhow, Result};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
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

    pub async fn spawn_next_from_ports(&self, ports: &[u16]) -> Result<String> {
        for port in ports {
            if self.active.lock().await.contains_key(port) {
                continue;
            }

            if tor_socks_reachable("127.0.0.1", *port).await {
                return Ok(self.register_proxy(*port, None).await);
            }

            return self.spawn_proxy_on_port(*port).await;
        }

        anyhow::bail!("no available Tor ports in provider range")
    }

    async fn spawn_proxy_on_port(&self, port: u16) -> Result<String> {
        let tor_exe = find_tor_exe()?;
        let data_dir = tor_data_root().join(format!("tor_data_{}", port));
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

        let mut child = cmd
            .spawn()
            .map_err(|e| anyhow!("Failed to spawn tor.exe: {}", e))?;
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

    /// Liveness probe for a registered Tor SOCKS port. The scale controller
    /// uses this to evict proxies whose tor process has died after registration
    /// (registration only confirms reachability once, at spawn time). A tor
    /// process's SOCKS listener stays up across circuit rebuilds, so a failure
    /// here means the process is genuinely gone.
    pub async fn is_socks_reachable(&self, port: u16) -> bool {
        tor_socks_reachable("127.0.0.1", port).await
    }

    pub fn subscribe(&self) -> watch::Receiver<Vec<String>> {
        self.proxy_receiver.clone()
    }

    pub async fn remove_proxy(&self, port: u16) -> Result<()> {
        let mut active = self.active.lock().await;
        match active.remove(&port) {
            Some(Some(mut child)) => {
                terminate_child_tree(&mut child).await;
                let _ = child.wait().await;
            }
            Some(None) => {
                if shutdown_via_control_port(port).await {
                    info!("Stopped control-managed Tor proxy on port {}", port);
                } else {
                    warn!(
                        "Removed externally managed Tor proxy {} from rotation only",
                        port
                    );
                }
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
        let mut known_ports = active.keys().copied().collect::<HashSet<_>>();
        for proxy in self.proxy_list.lock().await.iter() {
            if let Some(port) = proxy.rsplit(':').next().and_then(|p| p.parse::<u16>().ok()) {
                known_ports.insert(port);
            }
        }
        for (port, child) in active.drain() {
            if let Some(mut child) = child {
                terminate_child_tree(&mut child).await;
                let _ = child.wait().await;
                info!("Stopped Tor proxy on port {}", port);
            } else if shutdown_via_control_port(port).await {
                info!("Stopped control-managed Tor proxy on port {}", port);
            } else {
                warn!(
                    "Could not stop externally managed Tor proxy on port {}",
                    port
                );
            }
        }
        drop(active);

        for port in known_ports {
            if tor_socks_reachable("127.0.0.1", port).await {
                if shutdown_via_control_port(port).await {
                    info!("Stopped discovered Tor proxy on port {}", port);
                } else {
                    force_kill_tor_by_ports(port).await;
                }
            }
        }

        let mut list = self.proxy_list.lock().await;
        list.clear();
        let _ = self.proxy_sender.send(Vec::new());
        Ok(())
    }
}

async fn force_kill_tor_by_ports(socks_port: u16) {
    #[cfg(windows)]
    {
        let control_port = control_port_for_socks(socks_port);
        let filter = format!(
            "CommandLine like '%--SocksPort {}%' or CommandLine like '%--ControlPort {}%'",
            socks_port, control_port
        );
        let status = Command::new("wmic")
            .arg("process")
            .arg("where")
            .arg(filter)
            .arg("call")
            .arg("terminate")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .await;
        if let Err(e) = status {
            warn!("Failed to force-kill Tor on port {}: {}", socks_port, e);
        }
    }
}

async fn terminate_child_tree(child: &mut Child) {
    #[cfg(windows)]
    {
        if let Some(pid) = child.id() {
            let status = Command::new("taskkill")
                .arg("/PID")
                .arg(pid.to_string())
                .arg("/T")
                .arg("/F")
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status()
                .await;

            match status {
                Ok(exit) if exit.success() => return,
                Ok(exit) => {
                    warn!("taskkill returned {} for Tor process tree {}", exit, pid);
                }
                Err(e) => {
                    warn!("Failed to taskkill Tor process tree {}: {}", pid, e);
                }
            }

            let _ = child.kill().await;
            return;
        }
    }

    let _ = child.kill().await;
}

fn control_port_for_socks(port: u16) -> u16 {
    port.saturating_add(10_000)
}

fn cookie_path_for_port(port: u16) -> PathBuf {
    tor_data_root()
        .join(format!("tor_data_{}", port))
        .join("control_auth_cookie")
}

async fn shutdown_via_control_port(port: u16) -> bool {
    let cookie = match tokio::fs::read(cookie_path_for_port(port)).await {
        Ok(bytes) if !bytes.is_empty() => bytes,
        Ok(_) => {
            warn!("Tor control cookie was empty for port {}", port);
            return false;
        }
        Err(e) => {
            warn!("Failed to read Tor control cookie for port {}: {}", port, e);
            return false;
        }
    };

    let mut stream =
        match TcpStream::connect(format!("127.0.0.1:{}", control_port_for_socks(port))).await {
            Ok(stream) => stream,
            Err(e) => {
                warn!("Failed to connect to Tor control port for {}: {}", port, e);
                return false;
            }
        };

    let auth = format!("AUTHENTICATE {}\r\n", encode_cookie_hex(&cookie));
    if stream.write_all(auth.as_bytes()).await.is_err() {
        return false;
    }
    if !control_reply_ok(&mut stream).await {
        warn!("Tor AUTHENTICATE failed for port {}", port);
        return false;
    }

    if stream.write_all(b"SIGNAL SHUTDOWN\r\n").await.is_err() {
        return false;
    }
    if !control_reply_ok(&mut stream).await {
        warn!("Tor SIGNAL SHUTDOWN failed for port {}", port);
        return false;
    }

    let _ = stream.shutdown().await;
    true
}

fn encode_cookie_hex(cookie: &[u8]) -> String {
    cookie.iter().map(|b| format!("{:02X}", b)).collect()
}

async fn control_reply_ok(stream: &mut TcpStream) -> bool {
    let mut buf = [0u8; 256];
    match tokio::time::timeout(Duration::from_secs(2), stream.read(&mut buf)).await {
        Ok(Ok(n)) if n > 0 => std::str::from_utf8(&buf[..n])
            .map(|reply| reply.lines().any(|line| line.starts_with("250")))
            .unwrap_or(false),
        _ => false,
    }
}

async fn tor_socks_reachable(host: &str, port: u16) -> bool {
    TcpStream::connect(format!("{}:{}", host, port))
        .await
        .is_ok()
}

fn find_tor_exe() -> Result<PathBuf> {
    if let Ok(path) = std::env::var("TOR_BIN") {
        let path = PathBuf::from(path);
        if is_executable(&path) {
            return Ok(path);
        }
    }

    let exe_path = std::env::current_exe()?;
    let exe_dir = exe_path.parent().unwrap_or(Path::new("."));
    let mut candidates = vec![
        exe_dir.join("tor").join("tor.exe"),
        exe_dir.join("tor.exe"),
        exe_dir.join("tor").join("tor"),
        exe_dir.join("tor"),
        PathBuf::from("tor").join("tor.exe"),
        PathBuf::from("tor").join("tor"),
        PathBuf::from("tor.exe"),
        PathBuf::from("tor"),
    ];
    // On non-Windows, a committed `tor/tor.exe` is a Windows PE binary that
    // can't execute here (spawn fails with EACCES/ENOEXEC). Drop `.exe`
    // candidates so they can't shadow a real system tor discovered below.
    #[cfg(not(windows))]
    candidates.retain(|c| c.extension().and_then(|e| e.to_str()) != Some("exe"));

    for candidate in candidates {
        if is_executable(&candidate) {
            return Ok(candidate);
        }
    }

    if let Ok(path) = which::which("tor") {
        return Ok(path);
    }

    if let Ok(path) = which::which("tor.exe") {
        return Ok(path);
    }

    Err(anyhow!("tor binary not found in ./tor/, TOR_BIN, or PATH"))
}

/// True if `path` exists and is executable. On Unix this checks the execute
/// bits, so a non-executable file (e.g. a Windows `tor.exe` committed as
/// 100644) is rejected instead of being spawned and failing with EACCES. On
/// Windows, existence is sufficient — the OS handles `.exe` execution.
#[cfg(unix)]
fn is_executable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    match std::fs::metadata(path) {
        Ok(md) => md.is_file() && md.permissions().mode() & 0o111 != 0,
        Err(_) => false,
    }
}

#[cfg(not(unix))]
fn is_executable(path: &Path) -> bool {
    path.exists()
}

fn tor_data_root() -> PathBuf {
    std::env::var("LEECH_TOR_DATA_ROOT")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("."))
}
