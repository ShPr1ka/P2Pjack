//! Ядро: один UDP-сокет, таблица участников, пробивка NAT и маршрутизация пакетов
//! между виртуальным адаптером и сетью.
//!
//! Всё состояние принадлежит одной задаче (`Node::run`), поэтому блокировок нет.
//! Виртуальный адаптер подключается через пару каналов, что позволяет тестировать
//! узлы без драйвера и прав администратора.

use crate::crypto::NetKey;
use crate::proto::{self, Control, Msg, PROTO_VERSION, PeerAd};
use crate::stun;
use std::collections::hash_map::Entry;
use std::collections::{HashMap, HashSet};
use std::fmt;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::time::{Duration, Instant};
use tokio::net::UdpSocket;
use tokio::sync::{mpsc, oneshot, watch};
use tracing::{debug, info, warn};

/// Виртуальная сеть 10.77.0.0/16.
pub const NET_ADDR: Ipv4Addr = Ipv4Addr::new(10, 77, 0, 0);
pub const NET_PREFIX: u8 = 16;
pub const NET_BROADCAST: Ipv4Addr = Ipv4Addr::new(10, 77, 255, 255);
/// Порты, которые занимают узлы (по одному на сеть) и на которые шлётся поиск в физической локалке.
pub const LAN_PORTS: std::ops::RangeInclusive<u16> = 47777..=47786;

/// Сколько стучимся по новому адресу, прежде чем сдаться.
const PUNCH_WINDOW: Duration = Duration::from_secs(60);
/// Сколько не стучимся повторно по адресу, который не ответил.
const CANDIDATE_COOLDOWN: Duration = Duration::from_secs(60);
const PEER_TIMEOUT: Duration = Duration::from_secs(30);
const RELAY_TIMEOUT: Duration = Duration::from_secs(45);
/// Пока по текущему адресу участника идёт трафик, не переключаемся на другой.
const ADDR_STICKY: Duration = Duration::from_secs(10);

#[derive(Debug, Clone)]
pub enum Source {
    Manual,
    Dht,
    Gossip(String),
    Reconnect,
}

impl fmt::Display for Source {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Source::Manual => write!(f, "вручную"),
            Source::Dht => write!(f, "DHT"),
            Source::Gossip(name) => write!(f, "от {name}"),
            Source::Reconnect => write!(f, "переподключение"),
        }
    }
}

pub enum Event {
    /// Адрес, по которому может находиться участник сети. `persistent` — стучаться бесконечно.
    Candidate { addr: SocketAddr, source: Source, persistent: bool },
    Status(oneshot::Sender<Status>),
    Shutdown,
}

pub struct NodeConfig {
    pub id: u64,
    pub vip: Ipv4Addr,
    pub name: String,
    /// Искать участников широковещанием в той же физической локалке (на порты [`LAN_PORTS`]).
    pub lan_discovery: bool,
    pub stun_servers: Vec<SocketAddr>,
    /// Адреса, с которыми узел делает вид, что связи нет (для тестов ретрансляции).
    pub blocked: HashSet<SocketAddr>,
}

#[derive(Debug, Clone)]
pub struct PeerStatus {
    pub id: u64,
    pub name: String,
    pub vip: Ipv4Addr,
    pub addr: Option<SocketAddr>,
    pub rtt: Option<Duration>,
    /// Имя участника, через которого идёт трафик, если прямой связи нет.
    pub via: Option<String>,
}

#[derive(Debug, Clone)]
pub struct Status {
    pub name: String,
    pub vip: Ipv4Addr,
    pub public_addr: Option<SocketAddr>,
    pub peers: Vec<PeerStatus>,
    pub punching: usize,
}

struct Peer {
    id: u64,
    vip: Ipv4Addr,
    name: String,
    addr: SocketAddr,
    last_seen: Instant,
    addr_seen: Instant,
    rtt: Option<Duration>,
}

struct Relay {
    via: u64,
    ad: PeerAd,
    seen: Instant,
}

struct Candidate {
    source: Source,
    persistent: bool,
    since: Instant,
}

pub struct Node {
    cfg: NodeConfig,
    sock: UdpSocket,
    key: NetKey,
    start: Instant,
    ticks: u64,
    peers: HashMap<u64, Peer>,
    by_addr: HashMap<SocketAddr, u64>,
    by_vip: HashMap<Ipv4Addr, u64>,
    relays: HashMap<Ipv4Addr, Relay>,
    candidates: HashMap<SocketAddr, Candidate>,
    cooldown: HashMap<SocketAddr, Instant>,
    public_addr: watch::Sender<Option<SocketAddr>>,
    status: watch::Sender<Status>,
}

pub fn is_group(ip: Ipv4Addr) -> bool {
    ip.is_multicast() || ip.is_broadcast() || ip == NET_BROADCAST
}

pub fn in_network(ip: Ipv4Addr) -> bool {
    u32::from(ip) >> (32 - NET_PREFIX) == u32::from(NET_ADDR) >> (32 - NET_PREFIX)
}

fn is_private(addr: SocketAddr) -> bool {
    match addr.ip() {
        IpAddr::V4(ip) => ip.is_private() || ip.is_loopback() || ip.is_link_local(),
        IpAddr::V6(_) => false,
    }
}

impl Node {
    pub fn new(cfg: NodeConfig, sock: UdpSocket, key: NetKey) -> Self {
        let status = Status { name: cfg.name.clone(), vip: cfg.vip, public_addr: None, peers: Vec::new(), punching: 0 };
        Self {
            cfg,
            sock,
            key,
            start: Instant::now(),
            ticks: 0,
            peers: HashMap::new(),
            by_addr: HashMap::new(),
            by_vip: HashMap::new(),
            relays: HashMap::new(),
            candidates: HashMap::new(),
            cooldown: HashMap::new(),
            public_addr: watch::channel(None).0,
            status: watch::channel(status).0,
        }
    }

    /// Состояние узла: обновляется каждую секунду и при подключении/отключении участников.
    pub fn status_watch(&self) -> watch::Receiver<Status> {
        self.status.subscribe()
    }

    fn publish_status(&self) {
        self.status.send_replace(self.status());
    }

    /// Внешний адрес, который узнали через STUN.
    pub fn public_addr(&self) -> watch::Receiver<Option<SocketAddr>> {
        self.public_addr.subscribe()
    }

    /// `tun_rx` — пакеты, которые ОС отправила в виртуальный адаптер,
    /// `tun_tx` — пакеты, которые надо отдать ОС.
    pub async fn run(
        mut self,
        mut tun_rx: mpsc::Receiver<Vec<u8>>,
        tun_tx: mpsc::Sender<Vec<u8>>,
        mut events: mpsc::Receiver<Event>,
    ) {
        let mut buf = vec![0u8; 65536];
        let mut tick = tokio::time::interval(Duration::from_secs(1));
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            tokio::select! {
                r = self.sock.recv_from(&mut buf) => match r {
                    Ok((n, from)) => self.on_udp(&buf[..n], from, &tun_tx).await,
                    // Windows сообщает так об ICMP «порт недоступен» — при пробивке NAT это норма.
                    Err(e) => debug!("udp recv: {e}"),
                },
                Some(pkt) = tun_rx.recv() => self.on_tun(&pkt).await,
                ev = events.recv() => match ev {
                    Some(Event::Candidate { addr, source, persistent }) => {
                        self.add_candidate(addr, source, persistent)
                    }
                    Some(Event::Status(tx)) => {
                        let _ = tx.send(self.status());
                    }
                    Some(Event::Shutdown) | None => break,
                },
                _ = tick.tick() => self.on_tick().await,
            }
        }
        let bye = self.seal_control(&Control::Bye { id: self.cfg.id });
        for addr in self.peer_addrs() {
            self.send_raw(&bye, addr).await;
        }
    }

    // ---------- входящие пакеты ----------

    async fn on_udp(&mut self, data: &[u8], from: SocketAddr, tun_tx: &mpsc::Sender<Vec<u8>>) {
        if self.cfg.blocked.contains(&from) {
            return;
        }
        if stun::is_stun(data) {
            if self.cfg.stun_servers.contains(&from)
                && let Some(addr) = stun::parse_binding_response(data)
            {
                self.set_public_addr(addr);
            }
            return;
        }
        let Some(plain) = self.key.open(data) else {
            debug!("пакет от {from} не расшифрован (чужая сеть или другой пароль)");
            return;
        };
        match proto::decode(&plain) {
            Some(Msg::Data(pkt)) => self.on_data(pkt, from, tun_tx).await,
            Some(Msg::Control(c)) => self.on_control(c, from).await,
            None => {}
        }
    }

    async fn on_data(&mut self, pkt: &[u8], from: SocketAddr, tun_tx: &mpsc::Sender<Vec<u8>>) {
        let Some(dst) = proto::ipv4_dst(pkt) else { return };
        self.seen(from);
        if dst == self.cfg.vip || is_group(dst) {
            let _ = tun_tx.try_send(pkt.to_vec());
            return;
        }
        // Пакет не нам: ретранслируем участнику, до которого у отправителя нет прямой связи.
        let to = self.direct_addr(dst);
        if let Some(to) = to.filter(|to| *to != from) {
            let sealed = self.key.seal(&proto::encode_data(pkt));
            self.send_raw(&sealed, to).await;
        }
    }

    async fn on_control(&mut self, c: Control, from: SocketAddr) {
        match c {
            Control::Hello { version, id, vip, name, reply } => {
                if id == self.cfg.id {
                    // Наш собственный Hello вернулся (свой адрес из DHT или широковещание).
                    self.candidates.remove(&from);
                    self.cooldown.insert(from, Instant::now());
                    return;
                }
                if version != PROTO_VERSION {
                    debug!("{name} ({from}) использует другую версию протокола: {version}");
                    return;
                }
                // Ответ раньше, чем upsert_peer разошлёт список участников:
                // иначе новичок получит список от ещё незнакомого узла и выбросит его.
                if !reply {
                    let hello = self.seal_control(&self.hello(true));
                    self.send_raw(&hello, from).await;
                }
                self.upsert_peer(id, vip, name, from).await;
            }
            Control::Ping { id, t } => {
                if self.peers.contains_key(&id) {
                    self.by_addr.insert(from, id);
                    self.seen(from);
                    let pong = self.seal_control(&Control::Pong { t });
                    self.send_raw(&pong, from).await;
                } else {
                    // Он считает, что мы на связи, а мы его не знаем (например, перезапустились).
                    let hello = self.seal_control(&self.hello(false));
                    self.send_raw(&hello, from).await;
                }
            }
            Control::Pong { t } => {
                self.seen(from);
                let now_ms = self.now_ms();
                if let Some(p) = self.by_addr.get(&from).and_then(|id| self.peers.get_mut(id)) {
                    p.rtt = Some(Duration::from_millis(now_ms.saturating_sub(t)));
                }
            }
            Control::Peers(list) => {
                let Some(&via) = self.by_addr.get(&from) else { return };
                let Some(via_name) = self.peers.get(&via).map(|p| p.name.clone()) else { return };
                self.seen(from);
                let now = Instant::now();
                for ad in list {
                    if ad.id == self.cfg.id || self.peers.contains_key(&ad.id) || ad.vip == self.cfg.vip {
                        continue;
                    }
                    for &addr in &ad.addrs {
                        self.add_candidate(addr, Source::Gossip(via_name.clone()), false);
                    }
                    if !self.relays.contains_key(&ad.vip) {
                        info!("~ {} ({}) доступен через {via_name}, пробуем напрямую", ad.name, ad.vip);
                    }
                    self.relays.insert(ad.vip, Relay { via, ad, seen: now });
                }
            }
            Control::Bye { id } => {
                if self.by_addr.get(&from) == Some(&id) {
                    self.remove_peer(id, "вышел из сети");
                }
            }
        }
    }

    async fn on_tun(&mut self, pkt: &[u8]) {
        let Some(dst) = proto::ipv4_dst(pkt) else { return };
        if is_group(dst) {
            // IGMP — служебный трафик ОС, гонять его по сети незачем.
            if proto::ipv4_proto(pkt) == Some(2) {
                return;
            }
            let sealed = self.key.seal(&proto::encode_data(pkt));
            for addr in self.peer_addrs() {
                self.send_raw(&sealed, addr).await;
            }
            return;
        }
        let to = self.direct_addr(dst).or_else(|| {
            let relay = self.relays.get(&dst)?;
            self.peers.get(&relay.via).map(|p| p.addr)
        });
        if let Some(to) = to {
            let sealed = self.key.seal(&proto::encode_data(pkt));
            self.send_raw(&sealed, to).await;
        }
    }

    // ---------- таймер ----------

    async fn on_tick(&mut self) {
        self.ticks += 1;
        let now = Instant::now();

        // Внешний адрес: часто, пока не узнали, потом раз в минуту (заодно держим дыру в NAT).
        let stun_due = if self.public_addr.borrow().is_none() {
            self.ticks % 3 == 1
        } else {
            self.ticks.is_multiple_of(60)
        };
        if stun_due {
            for server in self.cfg.stun_servers.clone() {
                self.send_raw(&stun::binding_request(), server).await;
            }
        }

        // Пробивка NAT: шлём Hello всем кандидатам, пока не ответят или не истечёт окно.
        let mut targets = Vec::new();
        let mut expired = Vec::new();
        for (&addr, c) in &self.candidates {
            if self.by_addr.contains_key(&addr) {
                if !c.persistent {
                    expired.push(addr);
                }
            } else if c.persistent {
                if self.ticks.is_multiple_of(2) {
                    targets.push(addr);
                }
            } else if now - c.since > PUNCH_WINDOW {
                debug!("{addr} ({}) не ответил", c.source);
                expired.push(addr);
                self.cooldown.insert(addr, now);
            } else {
                targets.push(addr);
            }
        }
        for addr in expired {
            self.candidates.remove(&addr);
        }
        let hello = self.seal_control(&self.hello(false));
        for addr in targets {
            self.send_raw(&hello, addr).await;
        }
        if self.cfg.lan_discovery && self.ticks % 5 == 1 {
            for port in LAN_PORTS {
                self.send_raw(&hello, SocketAddr::from((Ipv4Addr::BROADCAST, port))).await;
            }
        }

        if self.ticks.is_multiple_of(5) {
            let ping = self.seal_control(&Control::Ping { id: self.cfg.id, t: self.now_ms() });
            for addr in self.peer_addrs() {
                self.send_raw(&ping, addr).await;
            }
        }
        if self.ticks.is_multiple_of(10) {
            self.gossip().await;
        }

        let dead: Vec<(u64, SocketAddr)> = self
            .peers
            .values()
            .filter(|p| now - p.last_seen > PEER_TIMEOUT)
            .map(|p| (p.id, p.addr))
            .collect();
        for (id, addr) in dead {
            self.remove_peer(id, "не отвечает");
            self.add_candidate(addr, Source::Reconnect, false);
        }
        self.relays.retain(|_, r| now - r.seen < RELAY_TIMEOUT);
        self.cooldown.retain(|_, t| now - *t < CANDIDATE_COOLDOWN);
        self.publish_status();
    }

    // ---------- участники ----------

    fn add_candidate(&mut self, addr: SocketAddr, source: Source, persistent: bool) {
        if addr.ip().is_unspecified() || addr.port() == 0 || !addr.is_ipv4() {
            return;
        }
        if self.cfg.blocked.contains(&addr)
            || *self.public_addr.borrow() == Some(addr)
            || self.sock.local_addr().ok() == Some(addr)
        {
            return;
        }
        if !persistent
            && (self.by_addr.contains_key(&addr)
                || self.cooldown.get(&addr).is_some_and(|t| t.elapsed() < CANDIDATE_COOLDOWN))
        {
            return;
        }
        match self.candidates.entry(addr) {
            Entry::Occupied(mut e) => e.get_mut().persistent |= persistent,
            Entry::Vacant(e) => {
                debug!("стучимся к {addr} ({source})");
                e.insert(Candidate { source, persistent, since: Instant::now() });
            }
        }
    }

    async fn upsert_peer(&mut self, id: u64, vip: Ipv4Addr, name: String, from: SocketAddr) {
        let now = Instant::now();
        self.by_addr.insert(from, id);
        if !self.candidates.get(&from).is_some_and(|c| c.persistent) {
            self.candidates.remove(&from);
        }
        if self.peers.contains_key(&id) {
            self.seen(from);
            return;
        }
        if vip == self.cfg.vip {
            warn!("У {name} такой же IP {vip}, как у вас! Задайте другой через --ip");
        } else if let Some(other) = self.by_vip.get(&vip).and_then(|o| self.peers.get(o)) {
            warn!("У {name} и {} одинаковый IP {vip}", other.name);
        }
        info!("+ {name} ({vip}) подключён напрямую, адрес {from}");
        self.relays.remove(&vip);
        self.by_vip.insert(vip, id);
        self.peers.insert(
            id,
            Peer { id, vip, name, addr: from, last_seen: now, addr_seen: now, rtt: None },
        );
        self.publish_status();
        // Сразу знакомим новичка с остальными, чтобы все пробивали NAT одновременно.
        self.gossip().await;
    }

    /// Отмечает, что от участника пришёл пакет с адреса `from`.
    fn seen(&mut self, from: SocketAddr) {
        let Some(p) = self.by_addr.get(&from).and_then(|id| self.peers.get_mut(id)) else {
            return;
        };
        let now = Instant::now();
        p.last_seen = now;
        if p.addr == from {
            p.addr_seen = now;
        } else if now - p.addr_seen > ADDR_STICKY || (is_private(from) && !is_private(p.addr)) {
            debug!("{}: адрес {} -> {from}", p.name, p.addr);
            p.addr = from;
            p.addr_seen = now;
        }
    }

    fn remove_peer(&mut self, id: u64, reason: &str) {
        let Some(p) = self.peers.remove(&id) else { return };
        info!("- {} ({}) {reason}", p.name, p.vip);
        if self.by_vip.get(&p.vip) == Some(&id) {
            self.by_vip.remove(&p.vip);
        }
        self.by_addr.retain(|_, v| *v != id);
        self.relays.retain(|_, r| r.via != id);
        self.publish_status();
    }

    async fn gossip(&mut self) {
        let ads: Vec<PeerAd> = self
            .peers
            .values()
            .map(|p| PeerAd {
                id: p.id,
                vip: p.vip,
                name: p.name.clone(),
                addrs: self.by_addr.iter().filter(|(_, v)| **v == p.id).map(|(a, _)| *a).collect(),
            })
            .collect();
        let targets: Vec<(u64, SocketAddr)> = self.peers.values().map(|p| (p.id, p.addr)).collect();
        for (id, addr) in targets {
            let list: Vec<PeerAd> = ads.iter().filter(|a| a.id != id).cloned().collect();
            if !list.is_empty() {
                let msg = self.seal_control(&Control::Peers(list));
                self.send_raw(&msg, addr).await;
            }
        }
    }

    // ---------- вспомогательное ----------

    fn direct_addr(&self, vip: Ipv4Addr) -> Option<SocketAddr> {
        self.by_vip.get(&vip).and_then(|id| self.peers.get(id)).map(|p| p.addr)
    }

    fn peer_addrs(&self) -> Vec<SocketAddr> {
        self.peers.values().map(|p| p.addr).collect()
    }

    fn hello(&self, reply: bool) -> Control {
        Control::Hello {
            version: PROTO_VERSION,
            id: self.cfg.id,
            vip: self.cfg.vip,
            name: self.cfg.name.clone(),
            reply,
        }
    }

    fn seal_control(&self, c: &Control) -> Vec<u8> {
        self.key.seal(&proto::encode_control(c))
    }

    async fn send_raw(&self, data: &[u8], to: SocketAddr) {
        if self.cfg.blocked.contains(&to) {
            return;
        }
        if let Err(e) = self.sock.send_to(data, to).await {
            debug!("отправка на {to}: {e}");
        }
    }

    fn set_public_addr(&mut self, addr: SocketAddr) {
        if *self.public_addr.borrow() != Some(addr) {
            info!("Ваш внешний адрес: {addr} (друг может подключиться вручную командой: add {addr})");
            self.public_addr.send_replace(Some(addr));
        }
    }

    fn now_ms(&self) -> u64 {
        self.start.elapsed().as_millis() as u64
    }

    fn status(&self) -> Status {
        let mut peers: Vec<PeerStatus> = self
            .peers
            .values()
            .map(|p| PeerStatus {
                id: p.id,
                name: p.name.clone(),
                vip: p.vip,
                addr: Some(p.addr),
                rtt: p.rtt,
                via: None,
            })
            .collect();
        peers.extend(self.relays.values().filter_map(|r| {
            let via = self.peers.get(&r.via)?;
            Some(PeerStatus {
                id: r.ad.id,
                name: r.ad.name.clone(),
                vip: r.ad.vip,
                addr: None,
                rtt: None,
                via: Some(via.name.clone()),
            })
        }));
        peers.sort_by_key(|p| p.vip);
        Status {
            name: self.cfg.name.clone(),
            vip: self.cfg.vip,
            public_addr: *self.public_addr.borrow(),
            peers,
            punching: self.candidates.keys().filter(|a| !self.by_addr.contains_key(a)).count(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::crypto::Secret;

    struct TestNode {
        addr: SocketAddr,
        vip: Ipv4Addr,
        os_in: mpsc::Sender<Vec<u8>>,
        os_out: mpsc::Receiver<Vec<u8>>,
        events: mpsc::Sender<Event>,
    }

    impl TestNode {
        fn connect_to(&self, other: &TestNode) {
            self.events
                .try_send(Event::Candidate { addr: other.addr, source: Source::Manual, persistent: true })
                .unwrap();
        }

        async fn status(&self) -> Status {
            let (tx, rx) = oneshot::channel();
            self.events.send(Event::Status(tx)).await.unwrap();
            rx.await.unwrap()
        }
    }

    async fn bind() -> UdpSocket {
        UdpSocket::bind("127.0.0.1:0").await.unwrap()
    }

    fn spawn(sock: UdpSocket, n: u8, key: &NetKey, blocked: &[SocketAddr]) -> TestNode {
        let addr = sock.local_addr().unwrap();
        let vip = Ipv4Addr::new(10, 77, 0, n);
        let cfg = NodeConfig {
            id: n as u64,
            vip,
            name: format!("node{n}"),
            lan_discovery: false,
            stun_servers: vec![],
            blocked: blocked.iter().copied().collect(),
        };
        let (os_in, tun_rx) = mpsc::channel(64);
        let (tun_tx, os_out) = mpsc::channel(64);
        let (events, ev_rx) = mpsc::channel(64);
        tokio::spawn(Node::new(cfg, sock, key.clone()).run(tun_rx, tun_tx, ev_rx));
        TestNode { addr, vip, os_in, os_out, events }
    }

    fn ip_packet(src: Ipv4Addr, dst: Ipv4Addr) -> Vec<u8> {
        let mut p = vec![0u8; 28];
        p[0] = 0x45;
        p[9] = 17;
        p[12..16].copy_from_slice(&src.octets());
        p[16..20].copy_from_slice(&dst.octets());
        p
    }

    /// Шлёт пакет из ОС узла `from`, пока он не выйдет из адаптера узла `to`.
    async fn delivers(from: &TestNode, to: &mut TestNode, within: Duration) -> bool {
        let deadline = Instant::now() + within;
        while Instant::now() < deadline {
            from.os_in.send(ip_packet(from.vip, to.vip)).await.unwrap();
            if let Ok(Some(p)) = tokio::time::timeout(Duration::from_millis(200), to.os_out.recv()).await {
                assert_eq!(proto::ipv4_dst(&p), Some(to.vip));
                return true;
            }
        }
        false
    }

    fn key(password: &str) -> NetKey {
        Secret::derive("test", password).unwrap().net_key()
    }

    #[tokio::test]
    async fn two_nodes_connect_and_exchange_packets() {
        let k = key("pw");
        let mut a = spawn(bind().await, 1, &k, &[]);
        let mut b = spawn(bind().await, 2, &k, &[]);
        a.connect_to(&b);
        assert!(delivers(&a, &mut b, Duration::from_secs(10)).await);
        assert!(delivers(&b, &mut a, Duration::from_secs(5)).await);
    }

    #[tokio::test]
    async fn wrong_password_does_not_connect() {
        let a = spawn(bind().await, 1, &key("pw"), &[]);
        let mut b = spawn(bind().await, 2, &key("другой"), &[]);
        a.connect_to(&b);
        b.connect_to(&a);
        assert!(!delivers(&a, &mut b, Duration::from_secs(3)).await);
        assert!(a.status().await.peers.is_empty());
    }

    #[tokio::test]
    async fn gossip_builds_full_mesh() {
        let k = key("pw");
        let a = spawn(bind().await, 1, &k, &[]);
        let b = spawn(bind().await, 2, &k, &[]);
        let mut c = spawn(bind().await, 3, &k, &[]);
        a.connect_to(&b);
        c.connect_to(&b);
        // A и C не знали друг о друге: их должен познакомить B.
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            let s = a.status().await;
            if s.peers.iter().any(|p| p.vip == c.vip && p.via.is_none()) {
                break;
            }
            assert!(Instant::now() < deadline, "A так и не подключился к C напрямую: {s:?}");
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        assert!(delivers(&a, &mut c, Duration::from_secs(5)).await);
    }

    #[tokio::test]
    async fn traffic_is_relayed_when_direct_path_fails() {
        let k = key("pw");
        let (sa, sb, sc) = (bind().await, bind().await, bind().await);
        let (addr_a, addr_c) = (sa.local_addr().unwrap(), sc.local_addr().unwrap());
        // Между A и C «симметричный NAT»: напрямую не пробиться.
        let mut a = spawn(sa, 1, &k, &[addr_c]);
        let b = spawn(sb, 2, &k, &[]);
        let mut c = spawn(sc, 3, &k, &[addr_a]);
        a.connect_to(&b);
        c.connect_to(&b);
        assert!(delivers(&a, &mut c, Duration::from_secs(10)).await);
        assert!(delivers(&c, &mut a, Duration::from_secs(5)).await);
        let s = a.status().await;
        let to_c = s.peers.iter().find(|p| p.vip == c.vip).unwrap();
        assert_eq!(to_c.via.as_deref(), Some("node2"));
    }
}
