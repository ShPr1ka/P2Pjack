//! Служба для графического интерфейса: кнопка питания, несколько сетей одновременно
//! на одном виртуальном адаптере, запоминание сетей и участников.
//!
//! Работает в отдельной задаче tokio. Интерфейс отправляет ей [`Cmd`] и читает [`Snapshot`].

use crate::config::{self, Config, KnownMember, SavedNetwork, Settings};
use crate::crypto::Secret;
use crate::discovery::{self, DhtConfig, UpnpMapping};
use crate::node::{self, Event, Node, NodeConfig, Source, Status};
use crate::{platform, proto, tun_dev};
use std::collections::{HashMap, HashSet};
use std::net::{Ipv4Addr, SocketAddr};
use std::sync::{Arc, Mutex, RwLock};
use std::time::Duration;
use tokio::sync::{mpsc, watch};
use tokio::task::JoinHandle;
use tracing::{info, warn};

pub enum Cmd {
    SetPower(bool),
    /// Создать сеть или присоединиться к ней: без сервера это одно и то же.
    Join { name: String, password: String },
    Leave(String),
    /// Ручное подключение к участнику сети по внешнему адресу.
    Connect { network: String, addr: SocketAddr },
    SetNick(String),
    SetSettings(Settings),
    /// Отключиться от всех (участники получат «вышел из сети») и завершить службу.
    Shutdown(std::sync::mpsc::Sender<()>),
}

#[derive(Debug, Clone, PartialEq)]
pub enum Power {
    Off,
    Starting,
    On,
    Failed(String),
}

#[derive(Debug, Clone, PartialEq)]
pub enum MemberState {
    Direct { rtt: Option<Duration> },
    Relay { via: String },
    Offline,
}

#[derive(Debug, Clone)]
pub struct MemberView {
    pub id: u64,
    pub name: String,
    pub vip: Ipv4Addr,
    pub state: MemberState,
    pub addr: Option<SocketAddr>,
}

#[derive(Debug, Clone)]
pub struct NetworkView {
    pub name: String,
    pub password: String,
    pub members: Vec<MemberView>,
    pub public_addr: Option<SocketAddr>,
    /// Сколько адресов сейчас пробуем (идёт поиск участников).
    pub searching: usize,
}

impl NetworkView {
    pub fn online(&self) -> usize {
        self.members.iter().filter(|m| m.state != MemberState::Offline).count()
    }
}

#[derive(Debug, Clone)]
pub struct Snapshot {
    pub power: Power,
    pub nick: String,
    pub vip: Ipv4Addr,
    pub public_addr: Option<SocketAddr>,
    pub upnp: Option<SocketAddr>,
    pub vpn_warning: bool,
    pub settings: Settings,
    pub networks: Vec<NetworkView>,
}

pub struct ServiceHandle {
    cmds: mpsc::Sender<Cmd>,
    pub snapshot: watch::Receiver<Snapshot>,
}

impl ServiceHandle {
    pub fn send(&self, cmd: Cmd) {
        if self.cmds.try_send(cmd).is_err() {
            warn!("служба не отвечает");
        }
    }

    /// Корректно отключается от сетей; ждёт не дольше `timeout`.
    pub fn shutdown(&self, timeout: Duration) {
        let (tx, rx) = std::sync::mpsc::channel();
        self.send(Cmd::Shutdown(tx));
        let _ = rx.recv_timeout(timeout);
    }
}

/// Запускает службу. `on_change` вызывается после каждого обновления снимка (например, для перерисовки окна).
pub fn start(rt: &tokio::runtime::Handle, on_change: impl Fn() + Send + Sync + 'static) -> anyhow::Result<ServiceHandle> {
    let id = config::load_node_id()?;
    let cfg = Config::load();
    let vip = config::derive_vip(id);
    let svc = Service {
        id,
        vip,
        power: Power::Off,
        active: None,
        vpn_warning: false,
        snap: watch::channel(snapshot_of(&cfg, vip)).0,
        cfg,
        on_change: Box::new(on_change),
    };
    let snapshot = svc.snap.subscribe();
    let (cmds, rx) = mpsc::channel(64);
    rt.spawn(svc.run(rx));
    Ok(ServiceHandle { cmds, snapshot })
}

/// Куда отдавать пакеты из адаптера: по одному звену на сеть.
struct Link {
    network: String,
    tx: mpsc::Sender<Vec<u8>>,
    status: watch::Receiver<Status>,
}

struct NetRuntime {
    events: mpsc::Sender<Event>,
    status: watch::Receiver<Status>,
    upnp: Arc<Mutex<Option<Arc<UpnpMapping>>>>,
    aux: JoinHandle<()>,
}

struct Active {
    _tun: tun_dev::TunGuard,
    tun_tx: mpsc::Sender<Vec<u8>>,
    links: Arc<RwLock<Vec<Link>>>,
    router: JoinHandle<()>,
    stun_servers: Vec<SocketAddr>,
    nets: HashMap<String, NetRuntime>,
}

struct Service {
    cfg: Config,
    id: u64,
    vip: Ipv4Addr,
    power: Power,
    active: Option<Active>,
    vpn_warning: bool,
    snap: watch::Sender<Snapshot>,
    on_change: Box<dyn Fn() + Send + Sync>,
}

impl Service {
    async fn run(mut self, mut cmds: mpsc::Receiver<Cmd>) {
        if self.cfg.power {
            self.power_on().await;
        }
        self.publish();
        let mut tick = tokio::time::interval(Duration::from_secs(1));
        loop {
            tokio::select! {
                cmd = cmds.recv() => match cmd {
                    Some(Cmd::Shutdown(ack)) => {
                        self.power_off(true).await;
                        let _ = ack.send(());
                        return;
                    }
                    Some(cmd) => self.handle(cmd).await,
                    None => {
                        self.power_off(true).await;
                        return;
                    }
                },
                _ = tick.tick() => {}
            }
            self.remember_members();
            self.publish();
        }
    }

    async fn handle(&mut self, cmd: Cmd) {
        match cmd {
            Cmd::SetPower(on) => {
                self.cfg.power = on;
                self.cfg.save();
                if on {
                    self.power_on().await;
                } else {
                    self.power_off(false).await;
                }
            }
            Cmd::Join { name, password } => {
                if self.cfg.networks.iter().any(|n| n.name == name) {
                    return;
                }
                info!("Добавлена сеть «{name}»");
                let net = SavedNetwork { name, password, members: Vec::new() };
                self.cfg.networks.push(net.clone());
                self.cfg.save();
                self.start_network(&net).await;
            }
            Cmd::Leave(name) => {
                info!("Покидаем сеть «{name}»");
                self.stop_network(&name);
                self.cfg.networks.retain(|n| n.name != name);
                self.cfg.save();
            }
            Cmd::Connect { network, addr } => {
                if let Some(rt) = self.active.as_ref().and_then(|a| a.nets.get(&network)) {
                    let ev = Event::Candidate { addr, source: Source::Manual, persistent: true };
                    let _ = rt.events.try_send(ev);
                    info!("«{network}»: стучимся к {addr}");
                }
            }
            Cmd::SetNick(nick) => {
                if nick != self.cfg.nick {
                    self.cfg.nick = nick;
                    self.cfg.save();
                    self.restart_networks().await;
                }
            }
            Cmd::SetSettings(settings) => {
                if settings != self.cfg.settings {
                    let reconnect = settings.network_differs(&self.cfg.settings);
                    self.cfg.settings = settings;
                    self.cfg.save();
                    if reconnect && self.active.is_some() {
                        self.power_off(false).await;
                        self.power_on().await;
                    }
                }
            }
            Cmd::Shutdown(_) => unreachable!("handled in run"),
        }
    }

    async fn power_on(&mut self) {
        if self.active.is_some() {
            return;
        }
        self.power = Power::Starting;
        self.publish();
        if !platform::is_elevated() {
            self.power = Power::Failed("нужны права администратора, чтобы создать сетевой адаптер".into());
            return;
        }
        let settings = self.cfg.settings.clone();
        let stun_servers = if settings.stun {
            let (servers, vpn) = discovery::resolve_stun_servers().await;
            self.vpn_warning = vpn;
            servers
        } else {
            Vec::new()
        };
        // Драйвер скачивается при первом запуске; качаем в отдельном потоке, чтобы не стопорить службу.
        #[cfg(windows)]
        match tokio::task::spawn_blocking(tun_dev::ensure_wintun).await {
            Ok(Ok(_)) => {}
            Ok(Err(e)) => {
                warn!("{e:#}");
                self.power = Power::Failed(format!("{e:#}"));
                return;
            }
            Err(e) => {
                self.power = Power::Failed(format!("{e}"));
                return;
            }
        }
        let tun = match tun_dev::create(self.vip) {
            Ok(t) => t,
            Err(e) => {
                warn!("{e:#}");
                self.power = Power::Failed(format!("{e:#}"));
                return;
            }
        };
        info!("Адаптер {} включён, ваш IP {}", tun_dev::ADAPTER_NAME, self.vip);
        platform::configure_adapter(settings.firewall, settings.mc_lan);

        let tun_dev::Tun { rx, tx, guard } = tun;
        let links = Arc::new(RwLock::new(Vec::new()));
        let router = tokio::spawn(route(rx, links.clone()));
        self.active = Some(Active {
            _tun: guard,
            tun_tx: tx,
            links,
            router,
            stun_servers,
            nets: HashMap::new(),
        });
        for net in self.cfg.networks.clone() {
            self.start_network(&net).await;
        }
        self.power = Power::On;
    }

    /// `exiting` — программа закрывается: дожидаемся снятия проброса портов на роутере.
    async fn power_off(&mut self, exiting: bool) {
        let names: Vec<String> = self.active.as_ref().map(|a| a.nets.keys().cloned().collect()).unwrap_or_default();
        let mut mappings = Vec::new();
        for name in names {
            mappings.extend(self.stop_network(&name));
        }
        if let Some(active) = self.active.take() {
            active.router.abort();
            info!("Адаптер {} выключен", tun_dev::ADAPTER_NAME);
        }
        self.power = Power::Off;
        let remove_all = async move {
            for m in mappings {
                m.remove().await;
            }
        };
        if exiting {
            // Даём узлам разослать «вышел из сети».
            tokio::time::sleep(Duration::from_millis(300)).await;
            let _ = tokio::time::timeout(Duration::from_secs(2), remove_all).await;
        } else {
            tokio::spawn(remove_all);
        }
        self.publish();
    }

    async fn restart_networks(&mut self) {
        let Some(active) = self.active.as_ref() else { return };
        let names: Vec<String> = active.nets.keys().cloned().collect();
        for name in &names {
            if let Some(m) = self.stop_network(name) {
                tokio::spawn(async move { m.remove().await });
            }
        }
        for net in self.cfg.networks.clone() {
            self.start_network(&net).await;
        }
    }

    async fn start_network(&mut self, net: &SavedNetwork) {
        let (id, vip, nick, settings) = (self.id, self.vip, self.cfg.nick.clone(), self.cfg.settings.clone());
        let Some(active) = self.active.as_mut() else { return };
        if active.nets.contains_key(&net.name) {
            return;
        }
        let secret = match Secret::derive(&net.name, &net.password) {
            Ok(s) => s,
            Err(e) => {
                warn!("«{}»: {e:#}", net.name);
                return;
            }
        };
        let sock = match discovery::bind_socket(None).await {
            Ok(s) => s,
            Err(e) => {
                warn!("«{}»: не удалось открыть UDP-порт: {e}", net.name);
                return;
            }
        };
        let local_port = sock.local_addr().map(|a| a.port()).unwrap_or_default();
        let node = Node::new(
            NodeConfig {
                id,
                vip,
                name: nick,
                lan_discovery: settings.lan,
                stun_servers: active.stun_servers.clone(),
                blocked: HashSet::new(),
            },
            sock,
            secret.net_key(),
        );
        let status = node.status_watch();
        let public = node.public_addr();
        let (node_tx, node_rx) = mpsc::channel(1024);
        let (events, ev_rx) = mpsc::channel(1024);
        tokio::spawn(node.run(node_rx, active.tun_tx.clone(), ev_rx));
        active.links.write().unwrap().push(Link {
            network: net.name.clone(),
            tx: node_tx,
            status: status.clone(),
        });

        // Проброс порта и DHT — в фоне: поиск роутера занимает до трёх секунд.
        let upnp = Arc::new(Mutex::new(None));
        let aux = tokio::spawn({
            let (upnp, events, name, topic) = (upnp.clone(), events.clone(), net.name.clone(), secret.dht_topic());
            async move {
                let mapping = if settings.upnp {
                    match UpnpMapping::create(local_port).await {
                        Ok(m) => {
                            info!("«{name}»: роутер открыл порт, внешний адрес {}", m.external);
                            Some(Arc::new(m))
                        }
                        Err(e) => {
                            info!("«{name}»: UPnP недоступен ({e:#}), обойдёмся пробивкой NAT");
                            None
                        }
                    }
                } else {
                    None
                };
                *upnp.lock().unwrap() = mapping.clone();
                if settings.dht {
                    let upnp_port = mapping.as_ref().map(|m| m.external.port());
                    discovery::spawn_dht(DhtConfig { topic, upnp_port, local_port }, public, events);
                }
                if let Some(m) = mapping {
                    loop {
                        tokio::time::sleep(Duration::from_secs(30 * 60)).await;
                        m.renew().await;
                    }
                }
            }
        });
        info!("Подключаемся к сети «{}» (порт {local_port})", net.name);
        active.nets.insert(net.name.clone(), NetRuntime { events, status, upnp, aux });
    }

    /// Останавливает узел сети; возвращает проброс порта, который надо снять с роутера.
    fn stop_network(&mut self, name: &str) -> Option<Arc<UpnpMapping>> {
        let active = self.active.as_mut()?;
        let rt = active.nets.remove(name)?;
        let _ = rt.events.try_send(Event::Shutdown);
        rt.aux.abort();
        active.links.write().unwrap().retain(|l| l.network != name);
        rt.upnp.lock().unwrap().take()
    }

    /// Запоминает участников, чтобы показывать их, даже когда они не в сети.
    fn remember_members(&mut self) {
        let Some(active) = self.active.as_ref() else { return };
        let mut changed = false;
        for net in &mut self.cfg.networks {
            let Some(rt) = active.nets.get(&net.name) else { continue };
            for p in &rt.status.borrow().peers {
                match net.members.iter_mut().find(|m| m.id == p.id) {
                    Some(m) if m.name == p.name && m.vip == p.vip => {}
                    Some(m) => {
                        m.name = p.name.clone();
                        m.vip = p.vip;
                        changed = true;
                    }
                    None => {
                        net.members.push(KnownMember { id: p.id, name: p.name.clone(), vip: p.vip });
                        changed = true;
                    }
                }
            }
        }
        if changed {
            self.cfg.save();
        }
    }

    fn publish(&self) {
        let mut snap = snapshot_of(&self.cfg, self.vip);
        snap.power = self.power.clone();
        snap.vpn_warning = self.vpn_warning;
        if let Some(active) = &self.active {
            for view in &mut snap.networks {
                if let Some(rt) = active.nets.get(&view.name) {
                    let status = rt.status.borrow();
                    merge_status(view, &status);
                    snap.public_addr = snap.public_addr.or(status.public_addr);
                    if snap.upnp.is_none() {
                        snap.upnp = rt.upnp.lock().unwrap().as_ref().map(|m| m.external);
                    }
                }
            }
        }
        self.snap.send_replace(snap);
        (self.on_change)();
    }
}

/// Снимок по одним только сохранённым настройкам: все участники «не в сети».
fn snapshot_of(cfg: &Config, vip: Ipv4Addr) -> Snapshot {
    Snapshot {
        power: Power::Off,
        nick: cfg.nick.clone(),
        vip,
        public_addr: None,
        upnp: None,
        vpn_warning: false,
        settings: cfg.settings.clone(),
        networks: cfg
            .networks
            .iter()
            .map(|n| NetworkView {
                name: n.name.clone(),
                password: n.password.clone(),
                members: n
                    .members
                    .iter()
                    .map(|m| MemberView {
                        id: m.id,
                        name: m.name.clone(),
                        vip: m.vip,
                        state: MemberState::Offline,
                        addr: None,
                    })
                    .collect(),
                public_addr: None,
                searching: 0,
            })
            .collect(),
    }
}

fn merge_status(view: &mut NetworkView, status: &Status) {
    view.public_addr = status.public_addr;
    view.searching = status.punching;
    for p in &status.peers {
        let state = match &p.via {
            Some(via) => MemberState::Relay { via: via.clone() },
            None => MemberState::Direct { rtt: p.rtt },
        };
        let member = MemberView { id: p.id, name: p.name.clone(), vip: p.vip, state, addr: p.addr };
        match view.members.iter_mut().find(|m| m.id == p.id) {
            Some(m) => *m = member,
            None => view.members.push(member),
        }
    }
    view.members.sort_by(|a, b| {
        (a.state == MemberState::Offline, a.name.to_lowercase()).cmp(&(b.state == MemberState::Offline, b.name.to_lowercase()))
    });
}

/// Раздаёт пакеты из адаптера узлам сетей: адресный — той сети, где есть получатель,
/// широковещательный — всем.
async fn route(mut rx: mpsc::Receiver<Vec<u8>>, links: Arc<RwLock<Vec<Link>>>) {
    while let Some(pkt) = rx.recv().await {
        dispatch(pkt, &links.read().unwrap());
    }
}

fn dispatch(pkt: Vec<u8>, links: &[Link]) {
    let Some(dst) = proto::ipv4_dst(&pkt) else { return };
    if node::is_group(dst) {
        for l in links {
            let _ = l.tx.try_send(pkt.clone());
        }
    } else if let Some(l) = links.iter().find(|l| l.status.borrow().peers.iter().any(|p| p.vip == dst)) {
        let _ = l.tx.try_send(pkt);
    }
}
