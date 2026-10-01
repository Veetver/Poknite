//! Identity for unpackaged Windows desktop notifications.
//!
//! Uses the documented Start-menu shortcut and no-COM/stub-CLSID option:
//! https://learn.microsoft.com/en-us/windows/win32/shell/enable-desktop-toast-with-appusermodelid
//! https://learn.microsoft.com/ru-ru/windows/apps/design/shell/tiles-and-notifications/toast-desktop-apps
//! Toast XML must use `activationType="protocol" launch="poknite://open"`.

use anyhow::{Context, Result, ensure};
use std::{
    ffi::{OsStr, OsString},
    os::windows::ffi::{OsStrExt, OsStringExt},
    path::Path,
    sync::{Mutex, OnceLock},
};
use windows::{
    Win32::{
        Foundation::RPC_E_CHANGED_MODE,
        Storage::EnhancedStorage::{PKEY_AppUserModel_ID, PKEY_AppUserModel_ToastActivatorCLSID},
        System::{
            Com::{
                CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED, CoCreateInstance, CoInitializeEx,
                CoTaskMemAlloc, CoUninitialize, IPersistFile, STGM_READ,
                StructuredStorage::{PROPVARIANT, PropVariantClear},
            },
            Variant::{VT_CLSID, VT_LPWSTR},
        },
        UI::Shell::{
            IShellLinkW, PropertiesSystem::IPropertyStore, SHStrDupW, SLGP_RAWPATH,
            SetCurrentProcessExplicitAppUserModelID, ShellLink,
        },
    },
    core::{GUID, Interface, PCWSTR, w},
};
use winreg::{RegKey, enums::HKEY_CURRENT_USER};

pub const APP_ID: &str = "Poknite.Desktop";
pub const OPEN_URI: &str = "poknite://open";

// A stable stub GUID, deliberately without any CLSID/LocalServer32 registration.
// Protocol activation handles clicks; the stub lets Action Center retain toasts.
const TOAST_STUB: GUID = GUID::from_u128(0x613cb3d6_72a3_4a6b_b452_90c5b7eddca1);
static REGISTERED: OnceLock<()> = OnceLock::new();
static REGISTRATION: Mutex<()> = Mutex::new(());

struct Apartment(bool);
impl Apartment {
    fn initialize() -> Result<Self> {
        let status = unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) };
        if status == RPC_E_CHANGED_MODE {
            // COM already belongs to an existing apartment on this thread.
            // Reuse it and never decrement a count that we did not acquire.
            return Ok(Self(false));
        }
        status.ok().context("Не удалось подготовить COM")?;
        // Both S_OK and S_FALSE require one matching CoUninitialize.
        Ok(Self(true))
    }
}
impl Drop for Apartment {
    fn drop(&mut self) {
        if self.0 {
            unsafe { CoUninitialize() }
        }
    }
}

struct Value(PROPVARIANT);
impl Value {
    fn text(text: &[u16]) -> Result<Self> {
        // SHStrDupW uses the COM task allocator, as required by PropVariantClear.
        let text = unsafe { SHStrDupW(PCWSTR(text.as_ptr())) }?;
        let mut value = Self(PROPVARIANT::default());
        unsafe {
            (*value.0.Anonymous.Anonymous).vt = VT_LPWSTR;
            (*value.0.Anonymous.Anonymous).Anonymous.pwszVal = text;
        }
        Ok(value)
    }
    fn guid(id: GUID) -> Result<Self> {
        let memory = unsafe { CoTaskMemAlloc(std::mem::size_of::<GUID>()) }.cast::<GUID>();
        ensure!(!memory.is_null(), "Недостаточно памяти для свойств ярлыка");
        let mut value = Self(PROPVARIANT::default());
        unsafe {
            memory.write(id);
            (*value.0.Anonymous.Anonymous).vt = VT_CLSID;
            (*value.0.Anonymous.Anonymous).Anonymous.puuid = memory;
        }
        Ok(value)
    }
    fn matches_text(&self, expected: &[u16]) -> bool {
        unsafe {
            if self.0.Anonymous.Anonymous.vt != VT_LPWSTR {
                return false;
            }
            let pointer = self.0.Anonymous.Anonymous.Anonymous.pwszVal;
            !pointer.is_null() && pointer.as_wide() == expected
        }
    }
    fn matches_guid(&self, expected: GUID) -> bool {
        unsafe {
            if self.0.Anonymous.Anonymous.vt != VT_CLSID {
                return false;
            }
            let pointer = self.0.Anonymous.Anonymous.Anonymous.puuid;
            !pointer.is_null() && *pointer == expected
        }
    }
}
impl Drop for Value {
    fn drop(&mut self) {
        unsafe {
            let _ = PropVariantClear(&mut self.0);
        }
    }
}

fn wide(value: &OsStr) -> Vec<u16> {
    value.encode_wide().chain(Some(0)).collect()
}

fn shortcut_matches(shortcut: &Path, executable: &Path) -> Result<bool> {
    let link: IShellLinkW = unsafe { CoCreateInstance(&ShellLink, None, CLSCTX_INPROC_SERVER) }?;
    let persist: IPersistFile = link.cast()?;
    let path = wide(shortcut.as_os_str());
    if unsafe { persist.Load(PCWSTR(path.as_ptr()), STGM_READ) }.is_err() {
        return Ok(false);
    }
    let mut target = vec![0u16; 32768];
    if unsafe { link.GetPath(&mut target, std::ptr::null_mut(), SLGP_RAWPATH.0 as u32) }.is_err() {
        return Ok(false);
    }
    let end = target.iter().position(|n| *n == 0).unwrap_or(target.len());
    let target = std::path::PathBuf::from(OsString::from_wide(&target[..end]));
    if target != executable {
        let same_file = matches!(
            (std::fs::canonicalize(&target), std::fs::canonicalize(executable)),
            (Ok(actual), Ok(expected)) if actual == expected
        );
        if !same_file {
            return Ok(false);
        }
    }
    let mut arguments = [0u16; 2];
    if unsafe { link.GetArguments(&mut arguments) }.is_err() || arguments[0] != 0 {
        return Ok(false);
    }
    let properties: IPropertyStore = link.cast()?;
    let Ok(id) = (unsafe { properties.GetValue(&PKEY_AppUserModel_ID) }) else {
        return Ok(false);
    };
    let id = Value(id);
    let expected: Vec<u16> = APP_ID.encode_utf16().collect();
    if !id.matches_text(&expected) {
        return Ok(false);
    }
    let Ok(activator) = (unsafe { properties.GetValue(&PKEY_AppUserModel_ToastActivatorCLSID) })
    else {
        return Ok(false);
    };
    Ok(Value(activator).matches_guid(TOAST_STUB))
}

fn create_shortcut(shortcut: &Path, executable: &Path) -> Result<()> {
    let link: IShellLinkW = unsafe { CoCreateInstance(&ShellLink, None, CLSCTX_INPROC_SERVER) }?;
    let target = wide(executable.as_os_str());
    let working = wide(
        executable
            .parent()
            .context("У программы нет каталога")?
            .as_os_str(),
    );
    unsafe {
        link.SetPath(PCWSTR(target.as_ptr()))?;
        link.SetArguments(w!(""))?;
        link.SetDescription(w!("Poknite — уведомления"))?;
        link.SetWorkingDirectory(PCWSTR(working.as_ptr()))?;
        link.SetIconLocation(PCWSTR(target.as_ptr()), 0)?;
    }
    let properties: IPropertyStore = link.cast()?;
    let identifier = wide(OsStr::new(APP_ID));
    let id = Value::text(&identifier)?;
    let activator = Value::guid(TOAST_STUB)?;
    unsafe {
        properties.SetValue(&PKEY_AppUserModel_ID, &id.0)?;
        properties.SetValue(&PKEY_AppUserModel_ToastActivatorCLSID, &activator.0)?;
        properties.Commit()?;
    }
    let persist: IPersistFile = link.cast()?;
    let temporary = shortcut.with_file_name(format!("Poknite.{}.lnk.tmp", std::process::id()));
    let path = wide(temporary.as_os_str());
    let result = (|| -> Result<()> {
        unsafe { persist.Save(PCWSTR(path.as_ptr()), false) }?;
        std::fs::rename(&temporary, shortcut)?;
        Ok(())
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(temporary);
    }
    result
}

fn register_protocol(executable: &Path) -> Result<()> {
    let (protocol, _) =
        RegKey::predef(HKEY_CURRENT_USER).create_subkey("Software\\Classes\\poknite")?;
    protocol.set_value("", &"URL:Poknite Protocol")?;
    protocol.set_value("URL Protocol", &"")?;
    let mut command = OsString::from("\"");
    command.push(executable.as_os_str());
    command.push("\"");
    // There is deliberately no `%1`: the only supported URI operation opens the
    // window. Arbitrary URI strings must not become --headless/--data-dir flags.
    let (open, _) = protocol.create_subkey("shell\\open\\command")?;
    open.set_value("", &command)?;
    let mut icon = command;
    icon.push(",0");
    let (icons, _) = protocol.create_subkey("DefaultIcon")?;
    icons.set_value("", &icon)?;
    Ok(())
}

/// Register once per process, repairing a stale shortcut after moving the EXE.
/// Call at GUI startup and before the first ToastNotifier is created.
pub fn register_app() -> Result<()> {
    let _registration = REGISTRATION
        .lock()
        .map_err(|_| anyhow::anyhow!("Регистрация Poknite прервана"))?;
    if REGISTERED.get().is_some() {
        return Ok(());
    }
    let executable = std::env::current_exe().context("Не удалось найти Poknite.exe")?;
    ensure!(
        !executable
            .as_os_str()
            .encode_wide()
            .any(|c| c == 0 || c == b'"' as u16),
        "Недопустимый путь Poknite.exe"
    );
    let roaming = std::env::var_os("APPDATA")
        .filter(|p| !p.is_empty())
        .context("Не найден каталог APPDATA")?;
    let directory = std::path::PathBuf::from(roaming)
        .join("Microsoft")
        .join("Windows")
        .join("Start Menu")
        .join("Programs");
    ensure!(
        directory.is_absolute(),
        "APPDATA должен быть абсолютным путём"
    );
    std::fs::create_dir_all(&directory)?;
    let _apartment = Apartment::initialize()?;
    unsafe { SetCurrentProcessExplicitAppUserModelID(w!("Poknite.Desktop")) }?;
    let shortcut = directory.join("Poknite.lnk");
    if !shortcut_matches(&shortcut, &executable)? {
        create_shortcut(&shortcut, &executable).context("Не удалось создать ярлык Poknite")?;
    }
    register_protocol(&executable).context("Не удалось зарегистрировать открытие Poknite")?;
    let _ = REGISTERED.set(());
    Ok(())
}
