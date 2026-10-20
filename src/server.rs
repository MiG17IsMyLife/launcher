//! 服务器在线状态探测：默认 sdgt.arcadehub.xyz（可在网络设置页修改），
//! 后台线程每 15 秒 TCP 探测一次（443 优先，失败回退 80），不阻塞 UI。

use std::net::{TcpStream, ToSocketAddrs};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

pub const SERVER_HOST: &str = "sdgt.arcadehub.xyz";
const PROBE_INTERVAL: Duration = Duration::from_secs(15);
const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
const PORTS: &[u16] = &[443, 80];

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ServerState {
    Unknown,
    Online,
    Offline,
}

pub struct ServerStatus {
    pub state: ServerState,
    pub last_check: Option<Instant>,
}

impl Default for ServerStatus {
    fn default() -> Self {
        ServerStatus {
            state: ServerState::Unknown,
            last_check: None,
        }
    }
}

/// 启动后台探测线程（立即探测一次，之后每 15 秒一次）。
/// `host` 为服务器地址共享槽：网络设置页改动后每轮探测自动使用新地址。
pub fn spawn_probe(shared: Arc<Mutex<ServerStatus>>, host: Arc<Mutex<String>>) {
    std::thread::spawn(move || {
        loop {
            let state = {
                let host = host
                    .lock()
                    .map(|h| h.clone())
                    .unwrap_or_else(|_| SERVER_HOST.to_string());
                let host = if host.trim().is_empty() {
                    SERVER_HOST.to_string()
                } else {
                    host
                };
                probe_once(&host)
            };
            if let Ok(mut s) = shared.lock() {
                s.state = state;
                s.last_check = Some(Instant::now());
            }
            std::thread::sleep(PROBE_INTERVAL);
        }
    });
}

fn probe_once(host: &str) -> ServerState {
    for port in PORTS {
        if let Ok(mut addrs) = (host, *port).to_socket_addrs() {
            while let Some(addr) = addrs.next() {
                if TcpStream::connect_timeout(&addr, CONNECT_TIMEOUT).is_ok() {
                    return ServerState::Online;
                }
            }
        }
    }
    ServerState::Offline
}
