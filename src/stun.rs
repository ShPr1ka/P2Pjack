//! Минимальный STUN-клиент (RFC 5389): узнаём свой внешний адрес и порт за NAT.
//!
//! Запросы уходят с того же UDP-сокета, что и трафик сети, поэтому ответ показывает
//! именно ту дыру в NAT, через которую к нам будут стучаться друзья.

use std::net::{Ipv4Addr, SocketAddr};

const COOKIE: u32 = 0x2112_A442;
const BINDING_REQUEST: u16 = 0x0001;
const BINDING_SUCCESS: u16 = 0x0101;
const ATTR_MAPPED_ADDRESS: u16 = 0x0001;
const ATTR_XOR_MAPPED_ADDRESS: u16 = 0x0020;

/// Бесплатные публичные STUN-серверы. Через них не идёт трафик — только узнаём свой адрес.
pub const DEFAULT_SERVERS: &[&str] = &[
    "stun.l.google.com:19302",
    "stun.cloudflare.com:3478",
    "stun1.l.google.com:19302",
];

pub fn binding_request() -> [u8; 20] {
    let mut r = [0u8; 20];
    r[0..2].copy_from_slice(&BINDING_REQUEST.to_be_bytes());
    r[4..8].copy_from_slice(&COOKIE.to_be_bytes());
    r[8..20].copy_from_slice(&rand::random::<[u8; 12]>());
    r
}

pub fn is_stun(b: &[u8]) -> bool {
    b.len() >= 20 && b[0] & 0xC0 == 0 && b[4..8] == COOKIE.to_be_bytes()
}

pub fn parse_binding_response(b: &[u8]) -> Option<SocketAddr> {
    if !is_stun(b) || u16::from_be_bytes([b[0], b[1]]) != BINDING_SUCCESS {
        return None;
    }
    let len = u16::from_be_bytes([b[2], b[3]]) as usize;
    let body = b.get(20..20 + len)?;
    let mut mapped = None;
    let mut i = 0;
    while i + 4 <= body.len() {
        let ty = u16::from_be_bytes([body[i], body[i + 1]]);
        let l = u16::from_be_bytes([body[i + 2], body[i + 3]]) as usize;
        let v = body.get(i + 4..i + 4 + l)?;
        // v[1] == 1 — IPv4
        if l >= 8 && v[1] == 1 {
            let port = u16::from_be_bytes([v[2], v[3]]);
            let ip = u32::from_be_bytes([v[4], v[5], v[6], v[7]]);
            match ty {
                ATTR_XOR_MAPPED_ADDRESS => {
                    let port = port ^ (COOKIE >> 16) as u16;
                    return Some(SocketAddr::from((Ipv4Addr::from(ip ^ COOKIE), port)));
                }
                ATTR_MAPPED_ADDRESS => mapped = Some(SocketAddr::from((Ipv4Addr::from(ip), port))),
                _ => {}
            }
        }
        i += 4 + l.div_ceil(4) * 4;
    }
    mapped
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_xor_mapped_address() {
        let addr: SocketAddr = "203.0.113.7:51234".parse().unwrap();
        let SocketAddr::V4(v4) = addr else { unreachable!() };
        let mut resp = vec![0x01, 0x01, 0, 12];
        resp.extend_from_slice(&COOKIE.to_be_bytes());
        resp.extend_from_slice(&[9; 12]);
        resp.extend_from_slice(&ATTR_XOR_MAPPED_ADDRESS.to_be_bytes());
        resp.extend_from_slice(&8u16.to_be_bytes());
        resp.extend_from_slice(&[0, 1]);
        resp.extend_from_slice(&(v4.port() ^ (COOKIE >> 16) as u16).to_be_bytes());
        resp.extend_from_slice(&(u32::from(*v4.ip()) ^ COOKIE).to_be_bytes());
        assert_eq!(parse_binding_response(&resp), Some(addr));
    }

    #[test]
    fn our_packets_are_not_stun() {
        assert!(!is_stun(&[b'P', b'J', 0, 0, 0x21, 0x12, 0xA4, 0x42, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]));
        assert!(is_stun(&binding_request()));
    }
}
