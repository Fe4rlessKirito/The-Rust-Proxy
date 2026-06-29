//! Tor daemon management for multiple instances.

use anyhow::{anyhow, Result};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;
use tokio::net::TcpStream;
use tokio::process::{Child, Command};
use tokio::time::sleep;
use tracing::{info, warn};

/// Spawn Tor instances on each given port.
/// Returns a list of child processes and a list of proxy URLs (socks5h://127.0.0.1:port).
pub async fn start_all_tor_instances(ports: &[u16]) -> Result<(Vec<Child>, Vec<String>)> {
    let mut children = Vec::new();
    let mut proxy_urls = Vec::new();

    for &port in ports {
        let url = format!("socks5h://127.0.0.1:{}", port);
        proxy_urls.push(url.clone());

        // Check if already running on this port
        if tor_socks_reachable("127.0.0.1", port).await {
            info!("Tor SOCKS proxy already running on port {}", port);
            // We don't spawn a new one; assume it's managed externally.
            continue;
        }

        // Find tor.exe
        let tor_exe = find_tor_exe()?;
        info!("Starting Tor on port {} from: {}", port, tor_exe.display());

        let data_dir = PathBuf::from(format!("tor_data_{}", port));
        if !data_dir.exists() {
            std::fs::create_dir_all(&data_dir)?;
        }

        let control_port = port + 1; // simple increment, may conflict; we can use a different strategy

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

        let mut child = cmd.spawn()?;

        // Wait for the SOCKS port to become ready
        let timeout = Duration::from_secs(15);
        let start = std::time::Instant::now();
        let mut ready = false;
        while start.elapsed() < timeout {
            if tor_socks_reachable("127.0.0.1", port).await {
                info!("Tor SOCKS proxy ready on port {}", port);
                ready = true;
                break;
            }
            sleep(Duration::from_millis(200)).await;
        }

        if !ready {
            warn!("Tor on port {} did not become ready within timeout", port);
            let _ = child.kill().await;
            // Continue with other ports; we'll skip this one.
            continue;
        }

        children.push(child);
    }

    if children.is_empty() && !proxy_urls.is_empty() {
        // If we didn't start any new instances, but we have proxy URLs, maybe they are already running.
        // We'll still return the URLs; the caller will use them.
        info!("Using existing Tor instances on ports: {:?}", proxy_urls);
    }

    Ok((children, proxy_urls))
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

/// Stop all Tor child processes.
pub async fn stop_all_tor(children: &mut Vec<Child>) -> Result<()> {
    for child in children.iter_mut() {
        if let Ok(Some(_)) = child.try_wait() {
            continue; // already exited
        }
        if let Err(e) = child.kill().await {
            warn!("Failed to kill Tor child: {}", e);
        }
        let _ = child.wait().await;
    }
    Ok(())
}
