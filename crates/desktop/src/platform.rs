use anyhow::{Result, bail};
use poknite_protocol::Message;
use std::{
    fs::{File, OpenOptions},
    io::{Read, Write},
    net::{TcpListener, TcpStream},
    path::Path,
    time::Duration,
};

/// Hold the lock for the lifetime of the process. Reopening tells the existing
/// process to show its window; a random local cookie prevents unsolicited IPC.
pub fn instance(directory: &Path, on_open: impl Fn() + Send + 'static) -> Result<Option<File>> {
    let path = directory.join("instance.lock");
    let mut opts = OpenOptions::new();
    opts.create(true).truncate(false).read(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    let mut file = opts.open(&path)?;
    if file.try_lock().is_err() {
        for _ in 0..20 {
            let contents = std::fs::read_to_string(&path).unwrap_or_default();
            let mut parts = contents.split_whitespace();
            if let (Some(port), Some(cookie)) = (parts.next(), parts.next())
                && let Ok(port) = port.parse::<u16>()
                && let Ok(mut stream) = TcpStream::connect_timeout(
                    &format!("127.0.0.1:{port}").parse()?,
                    Duration::from_millis(100),
                )
            {
                stream.write_all(cookie.as_bytes())?;
                return Ok(None);
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        bail!("Poknite уже запущен, но не отвечает")
    }
    let listener = TcpListener::bind("127.0.0.1:0")?;
    let cookie = uuid::Uuid::new_v4().to_string();
    file.set_len(0)?;
    writeln!(
        file,
        "{} {}",
        listener.local_addr()?.port(),
        cookie.replace(' ', "")
    )?;
    file.sync_all()?;
    let cookie = cookie.replace(' ', "");
    std::thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            let _ = stream.set_read_timeout(Some(Duration::from_millis(500)));
            let mut value = String::new();
            if stream.take(128).read_to_string(&mut value).is_ok() && value == cookie {
                on_open();
            }
        }
    });
    Ok(Some(file))
}
#[cfg(target_os = "linux")]
pub fn notify(messages: &[(Message, bool)], replaces: u32, quiet: bool) -> Result<u32> {
    if messages.is_empty() {
        return Ok(0);
    }
    let summary = messages.len() > 1 || messages.iter().any(|(_, replay)| *replay);
    let title = if summary {
        "Poknite".into()
    } else {
        format!("Poknite — {}", messages[0].0.sender_name)
    };
    let body = if summary {
        format!("Получено сообщений: {}", messages.len())
    } else {
        messages[0].0.text.clone()
    };
    crate::dbus::notify(&title, &body, replaces, quiet)
}
#[cfg(windows)]
pub fn notify(messages: &[(Message, bool)], _replaces: u32, quiet: bool) -> Result<u32> {
    use windows::{
        Data::Xml::Dom::XmlDocument,
        UI::Notifications::{ToastNotification, ToastNotificationManager},
        Win32::{
            Foundation::RPC_E_CHANGED_MODE,
            System::WinRT::{
                RO_INIT_MULTITHREADED, RO_INIT_SINGLETHREADED, RoInitialize, RoUninitialize,
            },
        },
        core::HSTRING,
    };
    if messages.is_empty() {
        return Ok(0);
    }
    register_app()?;
    unsafe {
        if let Err(error) = RoInitialize(RO_INIT_MULTITHREADED) {
            if error.code() != RPC_E_CHANGED_MODE {
                return Err(error.into());
            }
            RoInitialize(RO_INIT_SINGLETHREADED)?;
        }
    }
    struct Apartment;
    impl Drop for Apartment {
        fn drop(&mut self) {
            unsafe { RoUninitialize() }
        }
    }
    let _apartment = Apartment;
    let summary = messages.len() > 1 || messages.iter().any(|(_, replay)| *replay);
    let title = if summary {
        "Poknite".into()
    } else {
        format!("Poknite — {}", messages[0].0.sender_name)
    };
    let body = if summary {
        format!("Получено сообщений: {}", messages.len())
    } else {
        messages[0].0.text.clone()
    };
    let escape = |s: String| {
        s.replace('&', "&amp;")
            .replace('<', "&lt;")
            .replace('>', "&gt;")
            .replace('\"', "&quot;")
    };
    let xml = XmlDocument::new()?;
    xml.LoadXml(&HSTRING::from(format!("<toast activationType=\"protocol\" launch=\"{}\"><visual><binding template=\"ToastGeneric\"><text>{}</text><text>{}</text></binding></visual>{}</toast>",crate::windows_identity::OPEN_URI,escape(title),escape(body),if quiet {"<audio silent=\"true\"/>"} else {""})))?;
    let toast = ToastNotification::CreateToastNotification(&xml)?;
    toast.SetTag(&HSTRING::from(if summary {
        "summary".to_string()
    } else {
        messages[0].0.id.chars().take(16).collect()
    }))?;
    toast.SetGroup(&HSTRING::from("Poknite"))?;
    ToastNotificationManager::CreateToastNotifierWithId(&HSTRING::from(
        crate::windows_identity::APP_ID,
    ))?
    .Show(&toast)?;
    Ok(0)
}

#[cfg(windows)]
fn register_app() -> Result<()> {
    crate::windows_identity::register_app()?;
    use winreg::{RegKey, enums::HKEY_CURRENT_USER};
    let (key, _) = RegKey::predef(HKEY_CURRENT_USER)
        .create_subkey("Software\\Classes\\AppUserModelId\\Poknite.Desktop")?;
    key.set_value("DisplayName", &"Poknite")?;
    key.set_value(
        "IconUri",
        &std::env::current_exe()?.to_string_lossy().to_string(),
    )?;
    Ok(())
}
#[cfg(not(any(target_os = "linux", windows)))]
pub fn notify(_: &[(Message, bool)], _: u32, _: bool) -> Result<u32> {
    bail!("Платформа не поддерживается")
}

pub fn autostart(
    enabled: bool,
    directory: &Path,
    extra_ca: Option<&Path>,
    dev_http: bool,
) -> Result<()> {
    let exe = std::env::current_exe()?;
    #[cfg(not(windows))]
    let quote = |path: &Path| -> String {
        format!(
            "\"{}\"",
            path.to_string_lossy()
                .replace('\\', "\\\\")
                .replace('"', "\\\"")
                .replace('`', "\\`")
                .replace('$', "\\$")
                .replace('%', "%%")
        )
    };
    #[cfg(windows)]
    let quote = |path: &Path| -> String { format!("\"{}\"", path.to_string_lossy()) };
    let mut command = format!(
        "{} --background --data-dir {}",
        quote(&exe),
        quote(directory)
    );
    if let Some(ca) = extra_ca {
        command += &format!(" --ca {}", quote(ca));
    }
    if dev_http {
        command += " --dev-http";
    }
    #[cfg(target_os = "linux")]
    {
        let config = std::env::var_os("XDG_CONFIG_HOME")
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|| {
                std::path::PathBuf::from(std::env::var_os("HOME").unwrap_or_else(|| ".".into()))
                    .join(".config")
            });
        let folder = config.join("autostart");
        let file = folder.join("poknite.desktop");
        if enabled {
            std::fs::create_dir_all(folder)?;
            std::fs::write(
                file,
                format!(
                    "[Desktop Entry]\nType=Application\nName=Poknite\nExec={command}\nTerminal=false\nX-GNOME-Autostart-enabled=true\n"
                ),
            )?;
        } else if file.exists() {
            std::fs::remove_file(file)?;
        }
    }
    #[cfg(windows)]
    {
        use winreg::{RegKey, enums::HKEY_CURRENT_USER};
        let (key, _) = RegKey::predef(HKEY_CURRENT_USER)
            .create_subkey("Software\\Microsoft\\Windows\\CurrentVersion\\Run")?;
        if enabled {
            key.set_value("Poknite", &command)?;
        } else {
            match key.delete_value("Poknite") {
                Ok(()) => {}
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => return Err(e.into()),
            }
        }
    }
    Ok(())
}

#[cfg(target_os = "linux")]
pub fn cancel_notification(_key: &str, id: u32) -> Result<()> {
    if id > 0 {
        crate::dbus::close_notification(id)?;
    }
    Ok(())
}
#[cfg(windows)]
pub fn cancel_notification(key: &str, _id: u32) -> Result<()> {
    use windows::{UI::Notifications::ToastNotificationManager, core::HSTRING};
    let tag = if key == "summary" {
        key.to_string()
    } else {
        key.chars().take(16).collect()
    };
    ToastNotificationManager::History()?.RemoveGroupedTagWithId(
        &HSTRING::from(tag),
        &HSTRING::from("Poknite"),
        &HSTRING::from(crate::windows_identity::APP_ID),
    )?;
    Ok(())
}
#[cfg(not(any(target_os = "linux", windows)))]
pub fn cancel_notification(_key: &str, _id: u32) -> Result<()> {
    Ok(())
}
