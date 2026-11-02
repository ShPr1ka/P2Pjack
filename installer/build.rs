//! Вкладывает в установщик сжатые p2pjack.exe и wintun.dll и вшивает иконку.
//!
//! По умолчанию берёт их из `target/release` (сначала соберите программу: `scripts/build-installer.ps1`
//! делает всё по порядку). Пути можно переопределить переменными `P2PJACK_EXE` и `P2PJACK_WINTUN`.

use flate2::Compression;
use flate2::write::GzEncoder;
use std::io::Write;
use std::path::{Path, PathBuf};

fn main() {
    let root = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap()).join("..");
    let release = root.join("target").join("release");
    let out = PathBuf::from(std::env::var("OUT_DIR").unwrap());
    let input = |var: &str, file: &str| {
        println!("cargo:rerun-if-env-changed={var}");
        std::env::var_os(var).map(PathBuf::from).unwrap_or_else(|| release.join(file))
    };
    pack(&input("P2PJACK_EXE", "p2pjack.exe"), &out.join("p2pjack.exe.gz"), true);
    pack(&input("P2PJACK_WINTUN", "wintun.dll"), &out.join("wintun.dll.gz"), false);

    let icon = root.join("assets").join("p2pjack.ico");
    println!("cargo:rerun-if-changed={}", icon.display());
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        let mut res = winresource::WindowsResource::new();
        res.set_icon(&icon.to_string_lossy())
            .set("FileDescription", "Установщик P2PJack")
            .set("ProductName", "P2PJack");
        if let Err(e) = res.compile() {
            println!("cargo:warning=иконка не вшита в установщик: {e}");
        }
    }
}

/// Сжимает файл в `dst`. Если файла нет — кладёт пустой архив (установщик тогда сообщит об ошибке).
fn pack(src: &Path, dst: &Path, required: bool) {
    println!("cargo:rerun-if-changed={}", src.display());
    let data = std::fs::read(src).unwrap_or_else(|_| {
        if required {
            println!(
                "cargo:warning=нет {} — установщик соберётся пустым. Сначала: cargo build --release -p p2pjack",
                src.display()
            );
        }
        Vec::new()
    });
    let mut enc = GzEncoder::new(std::fs::File::create(dst).unwrap(), Compression::best());
    enc.write_all(&data).unwrap();
    enc.finish().unwrap();
}
