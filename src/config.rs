//! Настройки и запомненные сети: `%APPDATA%\p2pjack\` (или `~/.config/p2pjack/`).

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::net::Ipv4Addr;
use std::path::PathBuf;
use tracing::warn;

pub fn config_dir() -> PathBuf {
    let base = std::env::var_os("APPDATA")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))
        .unwrap_or_else(|| PathBuf::from("."));
    base.join("p2pjack")
}

/// Постоянный идентификатор этого компьютера: от него зависит виртуальный IP.
pub fn load_node_id() -> Result<u64> {
    let path = config_dir().join("node_id");
    if let Some(id) = std::fs::read_to_string(&path)
        .ok()
        .and_then(|s| u64::from_str_radix(s.trim(), 16).ok())
    {
        return Ok(id);
    }
    let id: u64 = rand::random();
    std::fs::create_dir_all(config_dir())?;
    std::fs::write(&path, format!("{id:016x}"))
        .with_context(|| format!("не удалось сохранить {}", path.display()))?;
    Ok(id)
}

pub fn derive_vip(id: u64) -> Ipv4Addr {
    let h = Sha256::digest(id.to_le_bytes());
    Ipv4Addr::new(10, 77, h[0], 1 + h[1] % 254)
}

pub fn default_nick() -> String {
    std::env::var("COMPUTERNAME")
        .or_else(|_| std::env::var("HOSTNAME"))
        .or_else(|_| std::env::var("USER"))
        .unwrap_or_else(|_| "Игрок".into())
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    /// Просить роутер открыть порт.
    pub upnp: bool,
    /// Искать участников через BitTorrent DHT.
    pub dht: bool,
    /// Узнавать внешний адрес через публичные STUN-серверы.
    pub stun: bool,
    /// Искать участников в физической локальной сети.
    pub lan: bool,
    /// Windows: частная сеть + правило брандмауэра для 10.77.0.0/16.
    pub firewall: bool,
    /// Windows: LAN-миры Minecraft через P2PJack.
    pub mc_lan: bool,
    /// Закрытие окна сворачивает программу в трей, а не отключает от сетей.
    pub tray: bool,
}

impl Settings {
    /// Отличаются ли настройки, от которых зависит подключение (а не только поведение окна).
    pub fn network_differs(&self, other: &Settings) -> bool {
        Settings { tray: other.tray, ..self.clone() } != *other
    }
}

impl Default for Settings {
    fn default() -> Self {
        Self { upnp: true, dht: true, stun: true, lan: true, firewall: true, mc_lan: true, tray: true }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct KnownMember {
    pub id: u64,
    pub name: String,
    pub vip: Ipv4Addr,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SavedNetwork {
    pub name: String,
    pub password: String,
    /// Кого видели в этой сети: показываются серыми, пока не в сети.
    #[serde(default)]
    pub members: Vec<KnownMember>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub nick: String,
    /// Включена ли программа (кнопка питания) — восстанавливается при запуске.
    pub power: bool,
    pub settings: Settings,
    pub networks: Vec<SavedNetwork>,
}

impl Default for Config {
    fn default() -> Self {
        Self { nick: default_nick(), power: true, settings: Settings::default(), networks: Vec::new() }
    }
}

impl Config {
    fn path() -> PathBuf {
        config_dir().join("config.json")
    }

    pub fn load() -> Self {
        match std::fs::read_to_string(Self::path()) {
            Ok(s) => serde_json::from_str(&s).unwrap_or_else(|e| {
                warn!("Настройки повреждены ({e}), начинаем с чистого листа");
                Self::default()
            }),
            Err(_) => Self::default(),
        }
    }

    pub fn save(&self) {
        let r = std::fs::create_dir_all(config_dir()).and_then(|_| {
            std::fs::write(Self::path(), serde_json::to_string_pretty(self).expect("config serializes"))
        });
        if let Err(e) = r {
            warn!("Не удалось сохранить настройки: {e}");
        }
    }
}
