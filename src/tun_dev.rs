//! Виртуальный сетевой адаптер (на Windows — драйвер Wintun от авторов WireGuard).

use crate::node::NET_PREFIX;
use anyhow::{Context, Result};
use std::net::Ipv4Addr;
use std::sync::Arc;
use tokio::sync::mpsc;
use tracing::warn;
use tun_rs::DeviceBuilder;

pub const ADAPTER_NAME: &str = "P2PJack";
/// 1500 минус заголовки IP/UDP, шифрование и запас под PPPoE.
pub const MTU: u16 = 1400;

pub struct Tun {
    /// Пакеты, которые ОС отправила в сеть через адаптер.
    pub rx: mpsc::Receiver<Vec<u8>>,
    /// Пакеты, которые надо отдать ОС.
    pub tx: mpsc::Sender<Vec<u8>>,
    /// Пока жив, адаптер существует в системе.
    pub guard: TunGuard,
}

pub struct TunGuard {
    tasks: Vec<tokio::task::JoinHandle<()>>,
}

/// Задачи чтения и записи держат последние ссылки на устройство: остановив их, удаляем адаптер.
impl Drop for TunGuard {
    fn drop(&mut self) {
        for t in &self.tasks {
            t.abort();
        }
    }
}

pub fn create(vip: Ipv4Addr) -> Result<Tun> {
    #[allow(unused_mut)]
    let mut builder = DeviceBuilder::new().name(ADAPTER_NAME).ipv4(vip, NET_PREFIX, None).mtu(MTU);
    #[cfg(windows)]
    {
        let dll = ensure_wintun()?;
        builder = builder.wintun_file(dll.to_string_lossy().into_owned()).description("P2PJack virtual LAN");
    }
    let dev = Arc::new(builder.build_async().context(
        "не удалось создать виртуальный адаптер (запустите программу от имени администратора)",
    )?);

    let (os_tx, rx) = mpsc::channel::<Vec<u8>>(1024);
    let reader = dev.clone();
    let read_task = tokio::spawn(async move {
        let mut buf = vec![0u8; 65536];
        loop {
            match reader.recv(&mut buf).await {
                Ok(n) => {
                    if os_tx.send(buf[..n].to_vec()).await.is_err() {
                        break;
                    }
                }
                Err(e) => {
                    warn!("чтение из адаптера: {e}");
                    break;
                }
            }
        }
    });

    let (tx, mut os_rx) = mpsc::channel::<Vec<u8>>(1024);
    let write_task = tokio::spawn(async move {
        while let Some(pkt) = os_rx.recv().await {
            if let Err(e) = dev.send(&pkt).await {
                warn!("запись в адаптер: {e}");
            }
        }
    });

    Ok(Tun { rx, tx, guard: TunGuard { tasks: vec![read_task, write_task] } })
}

/// Официальный архив Wintun и его SHA-256: скачанный файл сверяется с ним, подменить его нельзя.
#[cfg(windows)]
const WINTUN_URL: &str = "https://www.wintun.net/builds/wintun-0.14.1.zip";
#[cfg(windows)]
const WINTUN_ZIP_SHA256: &str = "07c256185d6ee3652e09fa55c0b673e2624b565e02c4b9091c79ca7d2f24ef51";

#[cfg(windows)]
fn wintun_places() -> Vec<std::path::PathBuf> {
    let mut places = Vec::new();
    if let Some(dir) = std::env::current_exe().ok().and_then(|p| p.parent().map(|d| d.to_path_buf())) {
        places.push(dir.join("wintun.dll"));
    }
    places.push(crate::config::config_dir().join("wintun.dll"));
    places
}

/// Путь к `wintun.dll`: рядом с программой или в папке настроек. Если его нет — скачивает.
/// Блокирует поток на время скачивания (≈1 МБ).
#[cfg(windows)]
pub fn ensure_wintun() -> Result<std::path::PathBuf> {
    let places = wintun_places();
    match places.iter().find(|p| p.is_file()) {
        Some(found) => Ok(found.clone()),
        None => download_wintun(&places),
    }
}

/// Скачивает и проверяет Wintun, сохраняет в первое место из `places`, куда удалось записать.
#[cfg(windows)]
fn download_wintun(places: &[std::path::PathBuf]) -> Result<std::path::PathBuf> {
    use sha2::{Digest, Sha256};
    use std::io::Read;

    tracing::info!("Драйвер Wintun не найден, скачиваем с wintun.net…");
    let zip = ureq::get(WINTUN_URL)
        .call()
        .context("не удалось скачать драйвер Wintun (нет интернета?)")?
        .body_mut()
        .with_config()
        .limit(8 << 20)
        .read_to_vec()
        .context("не удалось скачать драйвер Wintun")?;
    let hash = hex::encode(Sha256::digest(&zip));
    anyhow::ensure!(hash == WINTUN_ZIP_SHA256, "архив Wintun повреждён или подменён (SHA-256 {hash})");

    let arch = match std::env::consts::ARCH {
        "x86_64" => "amd64",
        "aarch64" => "arm64",
        "x86" => "x86",
        other => anyhow::bail!("Wintun не поддерживает архитектуру {other}"),
    };
    let mut archive = zip::ZipArchive::new(std::io::Cursor::new(zip)).context("архив Wintun не читается")?;
    let mut dll = Vec::new();
    archive
        .by_name(&format!("wintun/bin/{arch}/wintun.dll"))
        .context("в архиве Wintun нет нужной библиотеки")?
        .read_to_end(&mut dll)?;

    // Рядом с программой, а если туда нельзя писать (Program Files) — в папку настроек.
    for path in places {
        let tmp = path.with_extension("dll.part");
        let saved = path.parent().map_or(Ok(()), std::fs::create_dir_all).and_then(|_| {
            std::fs::write(&tmp, &dll)?;
            std::fs::rename(&tmp, path)
        });
        match saved {
            Ok(()) => {
                tracing::info!("Драйвер Wintun сохранён: {}", path.display());
                return Ok(path.clone());
            }
            Err(e) => {
                let _ = std::fs::remove_file(&tmp);
                warn!("Не удалось сохранить {}: {e}", path.display());
            }
        }
    }
    anyhow::bail!("не удалось сохранить wintun.dll ни рядом с программой, ни в папке настроек")
}

#[cfg(all(test, windows))]
mod tests {
    /// Ходит в интернет: `cargo test -- --ignored`
    #[test]
    #[ignore]
    fn downloads_and_verifies_wintun() {
        let dir = std::env::temp_dir().join(format!("p2pjack-wintun-test-{}", std::process::id()));
        let path = super::download_wintun(&[dir.join("wintun.dll")]).unwrap();
        let dll = std::fs::read(&path).unwrap();
        assert_eq!(&dll[..2], b"MZ", "это не Windows-библиотека");
        std::fs::remove_dir_all(dir).unwrap();
    }
}
