//! Lightweight STUN client for NAT traversal.
//!
//! Discovers our public IP:port by querying free STUN servers.
//! Implements just enough of RFC 5389 to get a MAPPED-ADDRESS response.

use std::net::SocketAddr;
use tokio::net::UdpSocket;
use tokio::time::{timeout, Duration};

/// Free public STUN servers.
const STUN_SERVERS: &[&str] = &[
    "stun.l.google.com:19302",
    "stun1.l.google.com:19302",
    "stun2.l.google.com:19302",
    "stun3.l.google.com:19302",
    "stun.cloudflare.com:3478",
];

/// STUN message types
const BINDING_REQUEST: u16 = 0x0001;
const BINDING_RESPONSE: u16 = 0x0101;

/// STUN magic cookie (RFC 5389)
const MAGIC_COOKIE: u32 = 0x2112A442;

/// STUN attribute types
const ATTR_MAPPED_ADDRESS: u16 = 0x0001;
const ATTR_XOR_MAPPED_ADDRESS: u16 = 0x0020;

/// Build a minimal STUN Binding Request (20 bytes).
fn build_binding_request() -> [u8; 20] {
    let mut buf = [0u8; 20];

    // Message Type: Binding Request
    buf[0..2].copy_from_slice(&BINDING_REQUEST.to_be_bytes());

    // Message Length: 0 (no attributes)
    buf[2..4].copy_from_slice(&0u16.to_be_bytes());

    // Magic Cookie
    buf[4..8].copy_from_slice(&MAGIC_COOKIE.to_be_bytes());

    // Transaction ID: 12 random bytes
    let txn_id: [u8; 12] = rand_bytes();
    buf[8..20].copy_from_slice(&txn_id);

    buf
}

/// Simple pseudo-random bytes using system time.
fn rand_bytes() -> [u8; 12] {
    use std::time::SystemTime;
    let seed = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let mut out = [0u8; 12];
    for (i, b) in out.iter_mut().enumerate() {
        *b = ((seed >> (i * 5)) & 0xFF) as u8;
    }
    out
}

/// Parse a STUN Binding Response to extract the XOR-MAPPED-ADDRESS or MAPPED-ADDRESS.
fn parse_binding_response(buf: &[u8]) -> Option<SocketAddr> {
    if buf.len() < 20 {
        return None;
    }

    // Check message type
    let msg_type = u16::from_be_bytes([buf[0], buf[1]]);
    if msg_type != BINDING_RESPONSE {
        return None;
    }

    // Check magic cookie
    let cookie = u32::from_be_bytes([buf[4], buf[5], buf[6], buf[7]]);
    if cookie != MAGIC_COOKIE {
        return None;
    }

    let msg_len = u16::from_be_bytes([buf[2], buf[3]]) as usize;
    let attrs = &buf[20..20 + msg_len.min(buf.len() - 20)];

    let mut offset = 0;
    while offset + 4 <= attrs.len() {
        let attr_type = u16::from_be_bytes([attrs[offset], attrs[offset + 1]]);
        let attr_len = u16::from_be_bytes([attrs[offset + 2], attrs[offset + 3]]) as usize;
        let attr_data = &attrs[offset + 4..offset + 4 + attr_len.min(attrs.len() - offset - 4)];

        match attr_type {
            ATTR_XOR_MAPPED_ADDRESS => {
                if let Some(addr) = parse_xor_mapped_address(attr_data, &buf[4..8]) {
                    return Some(addr);
                }
            }
            ATTR_MAPPED_ADDRESS => {
                if let Some(addr) = parse_mapped_address(attr_data) {
                    return Some(addr);
                }
            }
            _ => {}
        }

        // Attributes are padded to 4-byte boundary
        let padded_len = (attr_len + 3) & !3;
        offset += 4 + padded_len;
    }

    None
}

/// Parse XOR-MAPPED-ADDRESS attribute (RFC 5389 Section 15.2).
fn parse_xor_mapped_address(data: &[u8], magic: &[u8]) -> Option<SocketAddr> {
    if data.len() < 8 {
        return None;
    }

    let family = data[1];
    let xor_port = u16::from_be_bytes([data[2], data[3]]);
    let port = xor_port ^ (MAGIC_COOKIE >> 16) as u16;

    if family == 0x01 {
        // IPv4
        let ip_bytes = [
            data[4] ^ magic[0],
            data[5] ^ magic[1],
            data[6] ^ magic[2],
            data[7] ^ magic[3],
        ];
        let ip = std::net::Ipv4Addr::new(ip_bytes[0], ip_bytes[1], ip_bytes[2], ip_bytes[3]);
        Some(SocketAddr::new(std::net::IpAddr::V4(ip), port))
    } else {
        None // IPv6 not needed for our use case
    }
}

/// Parse MAPPED-ADDRESS attribute (RFC 5389 Section 15.1).
fn parse_mapped_address(data: &[u8]) -> Option<SocketAddr> {
    if data.len() < 8 {
        return None;
    }

    let family = data[1];
    let port = u16::from_be_bytes([data[2], data[3]]);

    if family == 0x01 {
        let ip = std::net::Ipv4Addr::new(data[4], data[5], data[6], data[7]);
        Some(SocketAddr::new(std::net::IpAddr::V4(ip), port))
    } else {
        None
    }
}

/// Discover our public IP:port as seen by STUN servers.
/// Uses the provided socket so the NAT mapping matches our game traffic port.
pub async fn discover_public_address(socket: &UdpSocket) -> Result<SocketAddr, String> {
    let request = build_binding_request();

    for server in STUN_SERVERS {
        // Resolve STUN server address
        let addrs: Vec<SocketAddr> = match tokio::net::lookup_host(server).await {
            Ok(addrs) => addrs.collect(),
            Err(_) => continue,
        };

        for addr in addrs {
            // Send binding request
            if socket.send_to(&request, addr).await.is_err() {
                continue;
            }

            // Wait for response (1.5s timeout per server)
            let mut buf = [0u8; 256];
            match timeout(Duration::from_millis(1500), socket.recv_from(&mut buf)).await {
                Ok(Ok((n, _))) => {
                    if let Some(public_addr) = parse_binding_response(&buf[..n]) {
                        return Ok(public_addr);
                    }
                }
                _ => continue,
            }
        }
    }

    Err("Failed to discover public address from any STUN server".to_string())
}

/// Perform UDP hole punching.
/// Both peers send packets to each other's public address simultaneously.
/// Returns true if hole punch succeeded (we received a response).
pub async fn hole_punch(
    socket: &UdpSocket,
    peer_public_addr: SocketAddr,
    attempts: u32,
) -> Result<bool, String> {
    let punch_packet = b"HOWL_PUNCH";

    for i in 0..attempts {
        // Send a punch packet to the peer's public address
        let _ = socket.send_to(punch_packet, peer_public_addr).await;

        // Check if we received anything from the peer
        let mut buf = [0u8; 64];
        match timeout(Duration::from_millis(200), socket.recv_from(&mut buf)).await {
            Ok(Ok((n, from_addr))) => {
                // Got a response! Could be from STUN or from peer
                if n >= punch_packet.len() && &buf[..punch_packet.len()] == punch_packet {
                    // It's a punch packet from our peer — hole punch succeeded!
                    return Ok(true);
                }
                // Could be a STUN response or other traffic, keep trying
            }
            _ => {
                // Timeout — try again with increasing delay
                tokio::time::sleep(Duration::from_millis(50 * (i as u64 + 1))).await;
            }
        }
    }

    Ok(false) // Hole punch didn't succeed in time
}

// ── Tauri Command ──

/// Discover our public address using STUN. Call after netplay_start (which binds the UDP socket).
#[tauri::command]
pub async fn stun_discover(
    netplay_state: tauri::State<'_, std::sync::Arc<std::sync::Mutex<crate::netplay::NetplayState>>>,
) -> Result<String, String> {
    // Get the socket from the netplay session
    let socket = {
        let ns = netplay_state.lock().map_err(|e| e.to_string())?;
        let session = ns.session.as_ref().ok_or("No netplay session active")?;
        session.socket.clone().ok_or("No UDP socket bound")?
    };

    let public_addr = discover_public_address(&socket).await?;
    Ok(public_addr.to_string())
}

/// Attempt UDP hole punching to a peer's public address.
#[tauri::command]
pub async fn stun_hole_punch(
    peer_address: String,
    netplay_state: tauri::State<'_, std::sync::Arc<std::sync::Mutex<crate::netplay::NetplayState>>>,
) -> Result<bool, String> {
    let peer_addr: SocketAddr = peer_address
        .parse()
        .map_err(|e| format!("Invalid peer address: {}", e))?;

    let socket = {
        let ns = netplay_state.lock().map_err(|e| e.to_string())?;
        let session = ns.session.as_ref().ok_or("No netplay session active")?;
        session.socket.clone().ok_or("No UDP socket bound")?
    };

    hole_punch(&socket, peer_addr, 20).await
}
