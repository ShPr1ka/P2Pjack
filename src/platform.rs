//! Всё, что зависит от Windows: права администратора, брандмауэр, маршрут для Minecraft.

use crate::node::{NET_ADDR, NET_PREFIX};
use crate::tun_dev::ADAPTER_NAME;
use tracing::{info, warn};

/// Запущены ли мы с правами администратора (без них не создать сетевой адаптер).
pub fn is_elevated() -> bool {
    #[cfg(windows)]
    {
        unsafe { windows_sys::Win32::UI::Shell::IsUserAnAdmin() != 0 }
    }
    #[cfg(not(windows))]
    {
        true
    }
}

/// Перезапускает программу с запросом прав администратора (окно UAC).
/// `true`, если новая копия запущена и эту можно закрывать.
#[cfg(windows)]
pub fn relaunch_elevated(args: &str) -> bool {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::UI::Shell::ShellExecuteW;
    use windows_sys::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;

    let Ok(exe) = std::env::current_exe() else { return false };
    let wide = |s: &std::ffi::OsStr| s.encode_wide().chain(Some(0)).collect::<Vec<u16>>();
    let verb = wide("runas".as_ref());
    let file = wide(exe.as_os_str());
    let params = wide(args.as_ref());
    let r = unsafe {
        ShellExecuteW(
            std::ptr::null_mut(),
            verb.as_ptr(),
            file.as_ptr(),
            params.as_ptr(),
            std::ptr::null(),
            SW_SHOWNORMAL,
        )
    };
    // Больше 32 — успех; иначе код ошибки (например, пользователь нажал «Нет» в UAC).
    r as isize > 32
}

/// Настройки Windows, без которых игры часто не видят друг друга.
/// Выполняется в фоне: Windows нужно несколько секунд, чтобы опознать новый адаптер.
pub fn configure_adapter(firewall: bool, mc_lan: bool) {
    if !cfg!(windows) || (!firewall && !mc_lan) {
        return;
    }
    std::thread::spawn(move || {
        std::thread::sleep(std::time::Duration::from_secs(4));
        let net = format!("{NET_ADDR}/{NET_PREFIX}");
        if firewall {
            run(
                "сеть P2PJack помечена как частная",
                "powershell",
                &[
                    "-NoProfile",
                    "-Command",
                    &format!("Set-NetConnectionProfile -InterfaceAlias '{ADAPTER_NAME}' -NetworkCategory Private"),
                ],
            );
            let _ = command("netsh")
                .args(["advfirewall", "firewall", "delete", "rule", "name=P2PJack"])
                .output();
            run(
                &format!("брандмауэр пропускает входящие подключения от {net}"),
                "netsh",
                &[
                    "advfirewall", "firewall", "add", "rule", "name=P2PJack", "dir=in", "action=allow",
                    &format!("remoteip={net}"),
                ],
            );
        }
        if mc_lan {
            // Minecraft объявляет LAN-миры на 224.0.2.60:4445 — направляем их в наш адаптер.
            run(
                "LAN-миры Minecraft будут видны через P2PJack",
                "netsh",
                &[
                    "interface", "ipv4", "add", "route", "prefix=224.0.2.60/32",
                    &format!("interface={ADAPTER_NAME}"),
                    "metric=1", "store=active",
                ],
            );
        }
    });
}

/// Открывает окно консоли с `ping -t` до участника.
pub fn open_ping_window(ip: std::net::Ipv4Addr) {
    #[cfg(windows)]
    {
        let _ = std::process::Command::new("cmd")
            .args(["/c", "start", "P2PJack ping", "ping", "-t", &ip.to_string()])
            .spawn();
    }
    #[cfg(not(windows))]
    let _ = ip;
}

/// Команда без мелькающего консольного окна.
pub fn command(program: &str) -> std::process::Command {
    #[allow(unused_mut)]
    let mut c = std::process::Command::new(program);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        c.creation_flags(CREATE_NO_WINDOW);
    }
    c
}

fn run(what: &str, program: &str, args: &[&str]) {
    match command(program).args(args).output() {
        Ok(out) if out.status.success() => info!("Windows: {what}"),
        Ok(out) => warn!(
            "Windows: не получилось ({what}): {}",
            String::from_utf8_lossy(&out.stderr).trim()
        ),
        Err(e) => warn!("Windows: не получилось ({what}): {e}"),
    }
}

// ---------- одна копия программы ----------

/// Порт на localhost, по которому копии P2PJack и установщик управляют запущенной программой.
pub const INSTANCE_PORT: u16 = 47799;

/// Отправляет запущенной копии команду (`show` или `quit`). `true`, если копия запущена.
pub fn signal_instance(cmd: &str) -> bool {
    if std::net::UdpSocket::bind(("127.0.0.1", INSTANCE_PORT)).is_ok() {
        return false;
    }
    if let Ok(s) = std::net::UdpSocket::bind("127.0.0.1:0") {
        let _ = s.send_to(cmd.as_bytes(), ("127.0.0.1", INSTANCE_PORT));
    }
    true
}

// ---------- автозапуск ----------

/// Имя задания в Планировщике. Через него, а не через ключ Run, потому что задание
/// запускает программу сразу с правами администратора — без окна UAC при каждом входе.
pub const AUTOSTART_TASK: &str = "P2PJack";

pub fn autostart_enabled() -> bool {
    cfg!(windows)
        && command("schtasks")
            .args(["/Query", "/TN", AUTOSTART_TASK])
            .output()
            .is_ok_and(|o| o.status.success())
}

/// Включает или выключает запуск `exe --autostart` при входе в Windows.
/// Создание задания с наивысшими правами требует прав администратора.
pub fn set_autostart(enable: bool, exe: &std::path::Path) -> anyhow::Result<()> {
    if !enable {
        if autostart_enabled() {
            schtasks(&["/Delete", "/TN", AUTOSTART_TASK, "/F"])?;
        }
        return Ok(());
    }
    let xml = autostart_task_xml(exe, &current_user(), "HighestAvailable");
    let path = std::env::temp_dir().join(format!("p2pjack-task-{}.xml", std::process::id()));
    write_utf16(&path, &xml)?;
    let r = schtasks(&["/Create", "/TN", AUTOSTART_TASK, "/XML", &path.to_string_lossy(), "/F"]);
    let _ = std::fs::remove_file(&path);
    r
}

fn current_user() -> String {
    match (std::env::var("USERDOMAIN"), std::env::var("USERNAME")) {
        (Ok(d), Ok(u)) => format!("{d}\\{u}"),
        (_, Ok(u)) => u,
        _ => String::new(),
    }
}

fn schtasks(args: &[&str]) -> anyhow::Result<()> {
    let out = command("schtasks").args(args).output()?;
    anyhow::ensure!(
        out.status.success(),
        "Планировщик заданий: {}",
        decode_console(&out.stderr).trim()
    );
    Ok(())
}

/// Консольные программы Windows пишут в OEM-кодировке; ASCII-часть при этом читается всегда.
fn decode_console(bytes: &[u8]) -> String {
    String::from_utf8(bytes.to_vec()).unwrap_or_else(|_| String::from_utf8_lossy(bytes).into_owned())
}

/// Задание: при входе пользователя, с наивысшими правами, без ограничений по батарее и времени работы.
fn autostart_task_xml(exe: &std::path::Path, user: &str, run_level: &str) -> String {
    let esc = |s: &str| s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;");
    let exe = esc(&exe.to_string_lossy());
    let user = esc(user);
    format!(
        r#"<?xml version="1.0" encoding="UTF-16"?>
<Task version="1.2" xmlns="http://schemas.microsoft.com/windows/2004/02/mit/task">
  <RegistrationInfo>
    <Description>P2PJack: виртуальная локальная сеть для игр</Description>
  </RegistrationInfo>
  <Triggers>
    <LogonTrigger>
      <Enabled>true</Enabled>
      <UserId>{user}</UserId>
      <Delay>PT5S</Delay>
    </LogonTrigger>
  </Triggers>
  <Principals>
    <Principal id="Author">
      <UserId>{user}</UserId>
      <LogonType>InteractiveToken</LogonType>
      <RunLevel>{run_level}</RunLevel>
    </Principal>
  </Principals>
  <Settings>
    <MultipleInstancesPolicy>IgnoreNew</MultipleInstancesPolicy>
    <DisallowStartIfOnBatteries>false</DisallowStartIfOnBatteries>
    <StopIfGoingOnBatteries>false</StopIfGoingOnBatteries>
    <AllowHardTerminate>true</AllowHardTerminate>
    <StartWhenAvailable>false</StartWhenAvailable>
    <RunOnlyIfNetworkAvailable>false</RunOnlyIfNetworkAvailable>
    <IdleSettings>
      <StopOnIdleEnd>false</StopOnIdleEnd>
      <RestartOnIdle>false</RestartOnIdle>
    </IdleSettings>
    <AllowStartOnDemand>true</AllowStartOnDemand>
    <Enabled>true</Enabled>
    <Hidden>false</Hidden>
    <RunOnlyIfIdle>false</RunOnlyIfIdle>
    <ExecutionTimeLimit>PT0S</ExecutionTimeLimit>
    <Priority>7</Priority>
  </Settings>
  <Actions Context="Author">
    <Exec>
      <Command>{exe}</Command>
      <Arguments>--autostart</Arguments>
    </Exec>
  </Actions>
</Task>
"#
    )
}

/// schtasks принимает XML только в UTF-16 с BOM.
fn write_utf16(path: &std::path::Path, text: &str) -> std::io::Result<()> {
    let mut bytes = vec![0xFF, 0xFE];
    bytes.extend(text.encode_utf16().flat_map(u16::to_le_bytes));
    std::fs::write(path, bytes)
}

#[cfg(all(test, windows))]
mod tests {
    use super::*;

    /// Регистрирует задание (без повышенных прав, под тестовым именем) и сразу удаляет:
    /// проверяет, что Планировщик принимает наш XML. `cargo test -- --ignored`
    #[test]
    #[ignore]
    fn task_scheduler_accepts_autostart_xml() {
        let name = format!("P2PJack-selftest-{}", std::process::id());
        let xml = autostart_task_xml(std::path::Path::new(r"C:\Program Files\P2PJack\p2pjack.exe"), &current_user(), "LeastPrivilege");
        let path = std::env::temp_dir().join(format!("{name}.xml"));
        write_utf16(&path, &xml).unwrap();
        let created = schtasks(&["/Create", "/TN", &name, "/XML", &path.to_string_lossy(), "/F"]);
        let _ = std::fs::remove_file(&path);
        let queried = schtasks(&["/Query", "/TN", &name]);
        let deleted = schtasks(&["/Delete", "/TN", &name, "/F"]);
        created.unwrap();
        queried.unwrap();
        deleted.unwrap();
    }
}
