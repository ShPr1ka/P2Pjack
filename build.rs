//! Вшивает иконку в exe на Windows (нужен rc.exe из Windows SDK; без него сборка идёт без иконки).

fn main() {
    println!("cargo:rerun-if-changed=assets/p2pjack.ico");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }
    let mut res = winresource::WindowsResource::new();
    res.set_icon("assets/p2pjack.ico")
        .set("FileDescription", "P2PJack — виртуальная локальная сеть для игр")
        .set("ProductName", "P2PJack");
    if let Err(e) = res.compile() {
        println!("cargo:warning=иконка не вшита в exe: {e}");
    }
}
