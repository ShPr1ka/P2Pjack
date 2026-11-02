//! Консольная версия P2PJack: одна сеть, управление из терминала.

use anyhow::{Context, Result, bail};
use clap::Parser;
use p2pjack::discovery::{self, DhtConfig, UpnpMapping};
use p2pjack::node::{self, Event, Node, NodeConfig, Source, Status};
use p2pjack::{config, crypto, platform, tun_dev};
use std::collections::HashSet;
use std::io::Write;
use std::net::Ipv4Addr;
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::sync::{mpsc, oneshot};
use tracing::{info, warn};

#[derive(Parser)]
#[command(
    name = "p2pjack-cli",
    version,
    about = "Виртуальная локальная сеть для игр через интернет, напрямую между игроками, без сервера"
)]
struct Args {
    /// Название сети (у всех участников одинаковое)
    #[arg(short, long)]
    network: Option<String>,
    /// Пароль сети (у всех участников одинаковый)
    #[arg(short, long)]
    password: Option<String>,
    /// Ваше имя, которое увидят остальные
    #[arg(long)]
    name: Option<String>,
    /// Виртуальный IP вида 10.77.x.y (по умолчанию постоянный, выбирается автоматически)
    #[arg(long)]
    ip: Option<Ipv4Addr>,
    /// UDP-порт для связи с участниками
    #[arg(long, default_value_t = 47777)]
    port: u16,
    /// Адрес участника для ручного подключения (можно несколько раз)
    #[arg(long = "peer", value_name = "IP:ПОРТ")]
    peers: Vec<String>,
    /// Не искать участников через BitTorrent DHT
    #[arg(long)]
    no_dht: bool,
    /// Не просить роутер открыть порт через UPnP
    #[arg(long)]
    no_upnp: bool,
    /// Не узнавать внешний адрес через публичные STUN-серверы
    #[arg(long)]
    no_stun: bool,
    /// Не искать участников в той же физической локальной сети
    #[arg(long)]
    no_lan: bool,
    /// Windows: сделать сеть P2PJack «частной» и разрешить входящие подключения от участников
    #[arg(long)]
    firewall: bool,
    /// Windows: показывать миры «Открыть для сети» Minecraft через P2PJack
    #[arg(long)]
    mc_lan: bool,
    /// Подробный журнал
    #[arg(short, long)]
    verbose: bool,
}

fn main() {
    let interactive = std::env::args().len() == 1;
    let result = tokio::runtime::Runtime::new()
        .context("tokio")
        .and_then(|rt| rt.block_on(run(Args::parse())));
    if let Err(e) = result {
        eprintln!("\nОшибка: {e:#}");
        if interactive {
            // Запустили двойным щелчком — не закрываем окно сразу.
            prompt("Нажмите Enter для выхода");
        }
        std::process::exit(1);
    }
}

async fn run(mut args: Args) -> Result<()> {
    let filter = if args.verbose { "p2pjack=debug" } else { "p2pjack=info" };
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| filter.into()),
        )
        .with_target(false)
        .with_timer(tracing_subscriber::fmt::time::uptime())
        .init();

    let network = args.network.take().unwrap_or_else(|| prompt("Название сети: "));
    let password = args.password.take().unwrap_or_else(|| prompt("Пароль: "));
    if network.is_empty() || password.is_empty() {
        bail!("нужны название сети и пароль");
    }
    if password.chars().count() < 8 {
        warn!("Пароль короче 8 символов: сеть легко подобрать. Лучше придумать длиннее.");
    }
    let name = args.name.take().unwrap_or_else(config::default_nick);

    let id = config::load_node_id()?;
    let vip = match args.ip {
        Some(ip) if node::in_network(ip) && ip != node::NET_BROADCAST && ip != node::NET_ADDR => ip,
        Some(ip) => bail!("--ip {ip}: адрес должен быть из сети {}/{}", node::NET_ADDR, node::NET_PREFIX),
        None => config::derive_vip(id),
    };

    info!("Вычисляем ключ сети...");
    let secret = crypto::Secret::derive(&network, &password)?;

    let sock = discovery::bind_socket(Some(args.port)).await?;
    let local_port = sock.local_addr()?.port();
    if local_port != args.port {
        warn!("Порт {} занят, используем {local_port}", args.port);
    }

    let upnp = if args.no_upnp {
        None
    } else {
        match UpnpMapping::create(local_port).await {
            Ok(m) => {
                info!("UPnP: роутер открыл порт, внешний адрес {}", m.external);
                Some(Arc::new(m))
            }
            Err(e) => {
                info!("UPnP недоступен: {e:#}. Обойдёмся пробивкой NAT");
                None
            }
        }
    };

    let stun_servers = if args.no_stun { Vec::new() } else { discovery::resolve_stun_servers().await.0 };

    #[cfg(windows)]
    tokio::task::spawn_blocking(tun_dev::ensure_wintun).await??;
    let tun = tun_dev::create(vip)?;

    let (events, ev_rx) = mpsc::channel::<Event>(1024);
    for peer in &args.peers {
        let addr = discovery::resolve(peer).await?;
        events.send(Event::Candidate { addr, source: Source::Manual, persistent: true }).await?;
    }

    let node = Node::new(
        NodeConfig {
            id,
            vip,
            name: name.clone(),
            lan_discovery: !args.no_lan,
            stun_servers,
            blocked: HashSet::new(),
        },
        sock,
        secret.net_key(),
    );

    if !args.no_dht {
        discovery::spawn_dht(
            DhtConfig { topic: secret.dht_topic(), upnp_port: upnp.as_ref().map(|m| m.external.port()), local_port },
            node.public_addr(),
            events.clone(),
        );
    }
    if let Some(m) = upnp.clone() {
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(Duration::from_secs(30 * 60)).await;
                m.renew().await;
            }
        });
    }
    if cfg!(windows) {
        platform::configure_adapter(args.firewall, args.mc_lan);
    } else if args.firewall || args.mc_lan {
        warn!("--firewall и --mc-lan работают только на Windows");
    }

    println!();
    println!("  P2PJack запущен. Сеть «{network}», вы — {name}");
    println!("  Ваш виртуальный IP: {vip}  (его сообщите друзьям для подключения в игре)");
    println!("  Команды: peers — список участников, add IP:ПОРТ — подключиться вручную, quit — выход");
    println!();

    tokio::spawn(console(events.clone()));
    let ctrl_c = events.clone();
    tokio::spawn(async move {
        if tokio::signal::ctrl_c().await.is_ok() {
            let _ = ctrl_c.send(Event::Shutdown).await;
        }
    });

    let tun_dev::Tun { rx, tx, guard: _guard } = tun;
    node.run(rx, tx, ev_rx).await;

    info!("Выходим...");
    if let Some(m) = upnp {
        let _ = tokio::time::timeout(Duration::from_secs(2), m.remove()).await;
    }
    Ok(())
}

async fn console(events: mpsc::Sender<Event>) {
    let mut lines = BufReader::new(tokio::io::stdin()).lines();
    while let Ok(Some(line)) = lines.next_line().await {
        let line = line.trim();
        let (cmd, arg) = line.split_once(' ').unwrap_or((line, ""));
        match cmd {
            "" => {}
            "p" | "peers" | "s" | "status" => {
                let (tx, rx) = oneshot::channel();
                if events.send(Event::Status(tx)).await.is_err() {
                    break;
                }
                if let Ok(s) = rx.await {
                    print_status(&s);
                }
            }
            "a" | "add" => match discovery::resolve(arg.trim()).await {
                Ok(addr) => {
                    println!("Стучимся к {addr}...");
                    let ev = Event::Candidate { addr, source: Source::Manual, persistent: true };
                    let _ = events.send(ev).await;
                }
                Err(e) => println!("{e:#}"),
            },
            "q" | "quit" | "exit" => {
                let _ = events.send(Event::Shutdown).await;
                break;
            }
            _ => println!("Команды: peers, add IP:ПОРТ, quit"),
        }
    }
}

fn print_status(s: &Status) {
    let public = s.public_addr.map_or("неизвестен".to_string(), |a| a.to_string());
    println!("\n  Вы: {} — {}   внешний адрес: {public}", s.name, s.vip);
    if s.peers.is_empty() {
        println!("  Пока никого нет. Ищем участников (адресов в работе: {})", s.punching);
    } else {
        println!("  {:<20} {:<15} {:<22} {:>8}  Связь", "Имя", "IP", "Адрес", "Пинг");
        for p in &s.peers {
            let addr = p.addr.map_or("—".into(), |a| a.to_string());
            let rtt = p.rtt.map_or("—".into(), |r| format!("{} мс", r.as_millis()));
            let link = p.via.as_ref().map_or("прямая".into(), |v| format!("через {v}"));
            println!("  {:<20} {:<15} {:<22} {:>8}  {link}", p.name, p.vip.to_string(), addr, rtt);
        }
    }
    println!();
}

fn prompt(text: &str) -> String {
    print!("{text}");
    let _ = std::io::stdout().flush();
    let mut s = String::new();
    let _ = std::io::stdin().read_line(&mut s);
    s.trim().to_string()
}
