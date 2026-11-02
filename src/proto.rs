//! Формат сообщений внутри зашифрованных пакетов.
//!
//! Первый байт открытого текста — тип: `0` — IP-пакет виртуальной сети как есть,
//! `1` — служебное сообщение в bincode.

use serde::{Deserialize, Serialize};
use std::net::{Ipv4Addr, SocketAddr};

pub const PROTO_VERSION: u16 = 1;

const T_DATA: u8 = 0;
const T_CONTROL: u8 = 1;

/// Сведения об участнике, которыми узлы делятся друг с другом.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PeerAd {
    pub id: u64,
    pub vip: Ipv4Addr,
    pub name: String,
    /// Все адреса, с которых этот участник был слышен.
    pub addrs: Vec<SocketAddr>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum Control {
    /// Рукопожатие и пробивка NAT. `reply` — ответ на чужой Hello (на него не отвечают).
    Hello { version: u16, id: u64, vip: Ipv4Addr, name: String, reply: bool },
    Ping { id: u64, t: u64 },
    Pong { t: u64 },
    /// Список прямых соседей отправителя: по нему остальные пробивают NAT друг к другу,
    /// а если не вышло — гонят трафик через отправителя.
    Peers(Vec<PeerAd>),
    Bye { id: u64 },
}

pub enum Msg<'a> {
    Data(&'a [u8]),
    Control(Control),
}

pub fn encode_data(packet: &[u8]) -> Vec<u8> {
    let mut v = Vec::with_capacity(packet.len() + 1);
    v.push(T_DATA);
    v.extend_from_slice(packet);
    v
}

pub fn encode_control(c: &Control) -> Vec<u8> {
    let mut v = vec![T_CONTROL];
    v.extend(bincode::serialize(c).expect("control message serializes"));
    v
}

pub fn decode(plain: &[u8]) -> Option<Msg<'_>> {
    let (&t, rest) = plain.split_first()?;
    match t {
        T_DATA => Some(Msg::Data(rest)),
        T_CONTROL => bincode::deserialize(rest).ok().map(Msg::Control),
        _ => None,
    }
}

/// Адрес назначения IPv4-пакета.
pub fn ipv4_dst(p: &[u8]) -> Option<Ipv4Addr> {
    (p.len() >= 20 && p[0] >> 4 == 4).then(|| Ipv4Addr::new(p[16], p[17], p[18], p[19]))
}

/// Номер протокола (6 — TCP, 17 — UDP, 2 — IGMP...).
pub fn ipv4_proto(p: &[u8]) -> Option<u8> {
    (p.len() >= 20 && p[0] >> 4 == 4).then(|| p[9])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn control_roundtrip() {
        let c = Control::Peers(vec![PeerAd {
            id: 7,
            vip: Ipv4Addr::new(10, 77, 1, 2),
            name: "Вася".into(),
            addrs: vec!["1.2.3.4:5".parse().unwrap()],
        }]);
        let enc = encode_control(&c);
        assert!(matches!(decode(&enc), Some(Msg::Control(d)) if d == c));
    }

    #[test]
    fn data_roundtrip() {
        let enc = encode_data(&[1, 2, 3]);
        assert!(matches!(decode(&enc), Some(Msg::Data(&[1, 2, 3]))));
    }
}
