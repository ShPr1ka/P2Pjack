//! P2PJack — виртуальная локальная сеть для игр через интернет без сервера
//! (аналог Hamachi / Radmin VPN).

pub mod config;
pub mod crypto;
pub mod discovery;
pub mod node;
pub mod platform;
pub mod proto;
pub mod service;
pub mod stun;
pub mod tun_dev;
pub mod ui_kit;
