//! РљР°Рє СѓС‡Р°СЃС‚РЅРёРєРё РЅР°С…РѕРґСЏС‚ РґСЂСѓРі РґСЂСѓРіР° Р±РµР· СЃРІРѕРµРіРѕ СЃРµСЂРІРµСЂР°:
//!
//! * **BitTorrent DHT** вЂ” РґРµС†РµРЅС‚СЂР°Р»РёР·РѕРІР°РЅРЅР°СЏ СЃРµС‚СЊ РёР· РјРёР»Р»РёРѕРЅРѕРІ СѓР·Р»РѕРІ С‚РѕСЂСЂРµРЅС‚-РєР»РёРµРЅС‚РѕРІ.
//!   РљР°Р¶РґС‹Р№ СѓС‡Р°СЃС‚РЅРёРє РѕР±СЉСЏРІР»СЏРµС‚ СЃРІРѕР№ РІРЅРµС€РЅРёР№ Р°РґСЂРµСЃ РїРѕРґ В«С‚РµРјРѕР№В», РІС‹РІРµРґРµРЅРЅРѕР№ РёР· РёРјРµРЅРё СЃРµС‚Рё
//!   Рё РїР°СЂРѕР»СЏ, Рё СЃРїСЂР°С€РёРІР°РµС‚ Р°РґСЂРµСЃР° РѕСЃС‚Р°Р»СЊРЅС‹С….
//! * **UPnP** вЂ” РїСЂРѕСЃРёРј СЂРѕСѓС‚РµСЂ РѕС‚РєСЂС‹С‚СЊ РїРѕСЂС‚, РµСЃР»Рё РѕРЅ СѓРјРµРµС‚.
//! * Р СѓС‡РЅРѕР№ Р°РґСЂРµСЃ (`--peer` / `add`) вЂ” РЅР° СЃР»СѓС‡Р°Р№, РєРѕРіРґР° DHT РЅРµРґРѕСЃС‚СѓРїРµРЅ.

use crate::node::{Event, LAN_PORTS, Source};
use anyhow::{Context, Result, bail};
use igd_next::PortMappingProtocol;
use igd_next::aio::Gateway;
use igd_next::aio::tokio::Tokio;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::time::{Duration, Instant};
use tokio::sync::{mpsc, watch};
use tracing::{debug, info, warn};

const UPNP_LEASE_SECS: u32 = 3600;
const DHT_LOOKUP_INTERVAL: Duration = Duration::from_secs(20);
const DHT_ANNOUNCE_INTERVAL: Duration = Duration::from_secs(5 * 60);

/// IP СЌС‚РѕРіРѕ РєРѕРјРїСЊСЋС‚РµСЂР° РІ С„РёР·РёС‡РµСЃРєРѕР№ Р»РѕРєР°Р»СЊРЅРѕР№ СЃРµС‚Рё. РџР°РєРµС‚С‹ РїСЂРё СЌС‚РѕРј РЅРµ РѕС‚РїСЂР°РІР»СЏСЋС‚СЃСЏ.
pub fn local_ipv4() -> Option<Ipv4Addr> {
    let s = std::net::UdpSocket::bind("0.0.0.0:0").ok()?;
    s.connect("8.8.8.8:80").ok()?;
    match s.local_addr().ok()?.ip() {
        IpAddr::V4(ip) if !ip.is_unspecified() => Some(ip),
        _ => None,
    }
}

/// РђРґСЂРµСЃ РїСЂРѕРІР°Р№РґРµСЂСЃРєРѕРіРѕ NAT (CGNAT, 100.64.0.0/10) РёР»Рё С‡Р°СЃС‚РЅС‹Р№ вЂ” РїСЂРѕР±СЂРѕСЃ РЅР° С‚Р°РєРѕРј Р±РµСЃРїРѕР»РµР·РµРЅ.
fn is_not_public(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(ip) => {
            ip.is_private() || ip.is_loopback() || ip.is_unspecified() || (ip.octets()[0] == 100 && ip.octets()[1] & 0xC0 == 64)
        }
        IpAddr::V6(_) => true,
    }
}

pub struct UpnpMapping {
    gateway: Gateway<Tokio>,
    local: SocketAddr,
    pub external: SocketAddr,
}

impl UpnpMapping {
    pub async fn create(port: u16) -> Result<Self> {
        let local_ip = local_ipv4().context("РЅРµ СѓРґР°Р»РѕСЃСЊ РѕРїСЂРµРґРµР»РёС‚СЊ Р»РѕРєР°Р»СЊРЅС‹Р№ IP")?;
        let local = SocketAddr::from((local_ip, port));
        let mut opts = igd_next::SearchOptions::default();
        opts.timeout = Some(Duration::from_secs(3));
        let gateway = igd_next::aio::tokio::search_gateway(opts).await.context("СЂРѕСѓС‚РµСЂ СЃ UPnP РЅРµ РЅР°Р№РґРµРЅ")?;
        let ext_ip = gateway.get_external_ip().await.context("СЂРѕСѓС‚РµСЂ РЅРµ СЃРѕРѕР±С‰РёР» РІРЅРµС€РЅРёР№ IP")?;
        if is_not_public(ext_ip) {
            bail!("Сѓ СЂРѕСѓС‚РµСЂР° РЅРµС‚ Р±РµР»РѕРіРѕ IP ({ext_ip}): РїСЂРѕРІР°Р№РґРµСЂ РёСЃРїРѕР»СЊР·СѓРµС‚ NAT, РїСЂРѕР±СЂРѕСЃ РЅРµ РїРѕРјРѕР¶РµС‚");
        }
        let ext_port = match gateway
            .add_port(PortMappingProtocol::UDP, port, local, UPNP_LEASE_SECS, "P2PJack")
            .await
        {
            Ok(()) => port,
            Err(_) => gateway
                .add_any_port(PortMappingProtocol::UDP, local, UPNP_LEASE_SECS, "P2PJack")
                .await
                .context("СЂРѕСѓС‚РµСЂ РѕС‚РєР°Р·Р°Р» РІ РїСЂРѕР±СЂРѕСЃРµ РїРѕСЂС‚Р°")?,
        };
        Ok(Self { gateway, local, external: SocketAddr::new(ext_ip, ext_port) })
    }

    pub async fn renew(&self) {
        let r = self
            .gateway
            .add_port(PortMappingProtocol::UDP, self.external.port(), self.local, UPNP_LEASE_SECS, "P2PJack")
            .await;
        if let Err(e) = r {
            warn!("UPnP: РЅРµ СѓРґР°Р»РѕСЃСЊ РїСЂРѕРґР»РёС‚СЊ РїСЂРѕР±СЂРѕСЃ РїРѕСЂС‚Р°: {e}");
        }
    }

    pub async fn remove(&self) {
        let _ = self.gateway.remove_port(PortMappingProtocol::UDP, self.external.port()).await;
    }
}

pub struct DhtConfig {
    pub topic: [u8; 20],
    /// Р’РЅРµС€РЅРёР№ РїРѕСЂС‚ РѕС‚ UPnP, РµСЃР»Рё РµСЃС‚СЊ: РѕРЅ РѕС‚РєСЂС‹С‚ РґР»СЏ РІС…РѕРґСЏС‰РёС… РЅР°РІРµСЂРЅСЏРєР°.
    pub upnp_port: Option<u16>,
    pub local_port: u16,
}

/// РџРѕРёСЃРє СѓС‡Р°СЃС‚РЅРёРєРѕРІ С‡РµСЂРµР· BitTorrent DHT РІ РѕС‚РґРµР»СЊРЅРѕРј РїРѕС‚РѕРєРµ.
pub fn spawn_dht(cfg: DhtConfig, public: watch::Receiver<Option<SocketAddr>>, events: mpsc::Sender<Event>) {
    let spawned = std::thread::Builder::new()
        .name("dht".into())
        .spawn(move || dht_loop(cfg, public, events));
    if let Err(e) = spawned {
        warn!("DHT РЅРµ Р·Р°РїСѓС‰РµРЅ: {e}");
    }
}

#[allow(deprecated)] // СЃРёРЅС…СЂРѕРЅРЅС‹Р№ API mainline: РЅР°Рј РЅСѓР¶РµРЅ СЃРІРѕР№ РїРѕС‚РѕРє, Р°СЃРёРЅС…СЂРѕРЅРЅС‹Р№ РЅРµ РґР°С‘С‚ РІС‹РіРѕРґС‹
fn dht_loop(cfg: DhtConfig, public: watch::Receiver<Option<SocketAddr>>, events: mpsc::Sender<Event>) {
    let dht = match mainline::Dht::client() {
        Ok(d) => d,
        Err(e) => {
            warn!("DHT РЅРµРґРѕСЃС‚СѓРїРµРЅ: {e}. РџРѕРґРєР»СЋС‡Р°Р№С‚РµСЃСЊ РІСЂСѓС‡РЅСѓСЋ (add IP:РџРћР Рў)");
            return;
        }
    };
    let topic = mainline::Id::from(cfg.topic);

    // РћР±СЉСЏРІР»СЏС‚СЊ РЅР°РґРѕ РІРЅРµС€РЅРёР№ РїРѕСЂС‚, РїРѕСЌС‚РѕРјСѓ СЃРЅР°С‡Р°Р»Р° Р¶РґС‘Рј РѕС‚РІРµС‚Р° STUN.
    let deadline = Instant::now() + Duration::from_secs(10);
    while cfg.upnp_port.is_none() && public.borrow().is_none() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(250));
    }

    let mut announced: Option<(Instant, u16)> = None;
    loop {
        if events.is_closed() {
            return;
        }
        let port = cfg
            .upnp_port
            .or_else(|| public.borrow().map(|a| a.port()))
            .unwrap_or(cfg.local_port);
        if announced.is_none_or(|(t, p)| p != port || t.elapsed() > DHT_ANNOUNCE_INTERVAL) {
            match dht.announce_peer(topic, Some(port)) {
                Ok(_) => {
                    if announced.is_none() {
                        info!("РЎРµС‚СЊ РѕР±СЉСЏРІР»РµРЅР° РІ DHT, РёС‰РµРј СѓС‡Р°СЃС‚РЅРёРєРѕРІ...");
                    }
                    announced = Some((Instant::now(), port));
                }
                Err(e) => warn!("DHT: РЅРµ СѓРґР°Р»РѕСЃСЊ РѕР±СЉСЏРІРёС‚СЊСЃСЏ: {e:?}"),
            }
        }

        let mut found = 0;
        for batch in dht.get_peers(topic) {
            for addr in batch {
                found += 1;
                let ev = Event::Candidate { addr: SocketAddr::V4(addr), source: Source::Dht, persistent: false };
                if events.blocking_send(ev).is_err() {
                    return;
                }
            }
        }
        debug!("DHT: РЅР°Р№РґРµРЅРѕ Р°РґСЂРµСЃРѕРІ: {found}");
        std::thread::sleep(DHT_LOOKUP_INTERVAL);
    }
}

/// Резолвит `хост:порт` в IPv4-адрес.
pub async fn resolve(s: &str) -> Result<SocketAddr> {
    tokio::net::lookup_host(s)
        .await
        .with_context(|| format!("не понял адрес «{s}», нужен формат IP:ПОРТ"))?
        .find(|a| a.is_ipv4())
        .with_context(|| format!("у «{s}» нет IPv4-адреса"))
}

/// Адреса STUN-серверов и признак того, что включён VPN/прокси в TUN-режиме.
///
/// DNS такого прокси (Clash, sing-box, Happ...) в режиме fake-IP отдаёт адреса из 198.18.0.0/15,
/// а весь трафик уводит через свои серверы: прямое P2P-соединение и DHT через него не работают.
pub async fn resolve_stun_servers() -> (Vec<SocketAddr>, bool) {
    let mut out = Vec::new();
    for s in crate::stun::DEFAULT_SERVERS {
        match tokio::time::timeout(Duration::from_secs(3), resolve(s)).await {
            Ok(Ok(a)) => out.push(a),
            _ => warn!("STUN-сервер {s} недоступен"),
        }
    }
    let vpn = out
        .iter()
        .any(|a| matches!(a.ip(), IpAddr::V4(ip) if ip.octets()[0] == 198 && ip.octets()[1] & 0xFE == 18));
    if vpn {
        warn!(
            "Похоже, включён VPN/прокси в TUN-режиме. Через него прямое P2P-соединение и поиск в DHT              не работают: выключите его или добавьте P2PJack в исключения (split tunneling)"
        );
    }
    (out, vpn)
}

/// UDP-сокет для сети: сначала `preferred`, затем свободный из [`LAN_PORTS`] (на них идёт поиск
/// в локальной сети), иначе любой.
pub async fn bind_socket(preferred: Option<u16>) -> std::io::Result<tokio::net::UdpSocket> {
    let ports = preferred.into_iter().chain(LAN_PORTS).chain([0]);
    let mut last_err = None;
    for port in ports {
        match tokio::net::UdpSocket::bind((Ipv4Addr::UNSPECIFIED, port)).await {
            Ok(sock) => {
                sock.set_broadcast(true)?;
                return Ok(sock);
            }
            Err(e) => last_err = Some(e),
        }
    }
    Err(last_err.expect("at least one port was tried"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::stun;

    /// РҐРѕРґРёС‚ РІ РЅР°СЃС‚РѕСЏС‰РёР№ РёРЅС‚РµСЂРЅРµС‚: `cargo test -- --ignored`
    #[tokio::test]
    #[ignore]
    async fn stun_and_dht_work_on_real_internet() {
        let _ = tracing_subscriber::fmt().with_env_filter("p2pjack=debug").with_test_writer().try_init();
        let sock = tokio::net::UdpSocket::bind("0.0.0.0:0").await.unwrap();
        let server = tokio::net::lookup_host(stun::DEFAULT_SERVERS[0]).await.unwrap().find(|a| a.is_ipv4()).unwrap();
        sock.send_to(&stun::binding_request(), server).await.unwrap();
        let mut buf = [0u8; 512];
        let (n, _) = tokio::time::timeout(Duration::from_secs(5), sock.recv_from(&mut buf)).await.unwrap().unwrap();
        let public = stun::parse_binding_response(&buf[..n]).expect("STUN РѕС‚РІРµС‚РёР»");
        println!("РІРЅРµС€РЅРёР№ Р°РґСЂРµСЃ: {public}");

        let (_pub_tx, pub_rx) = watch::channel(Some(public));
        let (tx, mut rx) = mpsc::channel(256);
        let topic = rand::random::<[u8; 20]>();
        spawn_dht(DhtConfig { topic, upnp_port: None, local_port: 0 }, pub_rx, tx);
        let found = tokio::time::timeout(Duration::from_secs(120), async {
            while let Some(Event::Candidate { addr, .. }) = rx.recv().await {
                if addr == public {
                    return true;
                }
            }
            false
        })
        .await;
        assert_eq!(found, Ok(true), "DHT РЅРµ РІРµСЂРЅСѓР» РЅР°С€ СЃРѕР±СЃС‚РІРµРЅРЅС‹Р№ Р°РґСЂРµСЃ");
    }
}
