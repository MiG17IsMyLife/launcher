use std::net::{TcpStream, ToSocketAddrs};
use std::time::Duration;

pub const SERVER_HOST: &str = "sdgt.arcadehub.xyz";

pub const PROBE_INTERVAL: Duration = Duration::from_secs(15);
const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
const PORTS: &[u16] = &[443, 80];

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ServerState {
    Unknown,
    Online,
    Offline,
}

pub fn probe_now(host: &str) -> ServerState {
    let host = host.trim();
    let host = if host.is_empty() { SERVER_HOST } else { host };
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
