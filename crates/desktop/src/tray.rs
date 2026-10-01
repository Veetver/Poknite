//! Windows notification area icon with an independent native message window.
use anyhow::{Result, bail};
use windows_sys::Win32::{
    Foundation::*,
    System::LibraryLoader::*,
    UI::{Shell::*, WindowsAndMessaging::*},
};
struct Callbacks {
    open: Box<dyn Fn() + Send>,
    exit: Box<dyn Fn() + Send>,
}
pub struct Tray(usize);
impl Drop for Tray {
    fn drop(&mut self) {
        unsafe {
            PostMessageW(self.0 as HWND, WM_CLOSE, 0, 0);
        }
    }
}
fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(Some(0)).collect()
}
fn icon(window: HWND) -> NOTIFYICONDATAW {
    let mut icon: NOTIFYICONDATAW = unsafe { std::mem::zeroed() };
    icon.cbSize = std::mem::size_of::<NOTIFYICONDATAW>() as u32;
    icon.hWnd = window;
    icon.uID = 1;
    icon.uFlags = NIF_ICON | NIF_MESSAGE | NIF_TIP;
    icon.uCallbackMessage = WM_APP + 1;
    icon.hIcon = unsafe { LoadIconW(std::ptr::null_mut(), IDI_INFORMATION) };
    for (i, v) in wide("Poknite — двойной щелчок открывает окно")
        .into_iter()
        .take(128)
        .enumerate()
    {
        icon.szTip[i] = v;
    }
    icon
}
unsafe extern "system" fn procedure(
    window: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    unsafe {
        if message == RegisterWindowMessageW(wide("TaskbarCreated").as_ptr()) {
            Shell_NotifyIconW(NIM_ADD, &icon(window));
            return 0;
        }
        let callbacks = GetWindowLongPtrW(window, GWLP_USERDATA) as *mut Callbacks;
        match message {
            value if value == WM_APP + 1 && !callbacks.is_null() => {
                if lparam as u32 == WM_LBUTTONDBLCLK {
                    ((*callbacks).open)()
                }
                if lparam as u32 == WM_RBUTTONUP {
                    let mut point: POINT = std::mem::zeroed();
                    GetCursorPos(&mut point);
                    SetForegroundWindow(window);
                    let menu = CreatePopupMenu();
                    AppendMenuW(menu, MF_STRING, 1, wide("Открыть").as_ptr());
                    AppendMenuW(menu, MF_STRING, 2, wide("Выход").as_ptr());
                    let command = TrackPopupMenu(
                        menu,
                        TPM_RETURNCMD | TPM_NONOTIFY,
                        point.x,
                        point.y,
                        0,
                        window,
                        std::ptr::null(),
                    );
                    DestroyMenu(menu);
                    if command == 1 {
                        ((*callbacks).open)()
                    } else if command == 2 {
                        ((*callbacks).exit)()
                    }
                    PostMessageW(window, WM_NULL, 0, 0);
                }
                0
            }
            WM_CLOSE => {
                Shell_NotifyIconW(NIM_DELETE, &icon(window));
                DestroyWindow(window);
                0
            }
            WM_DESTROY => {
                PostQuitMessage(0);
                0
            }
            WM_NCDESTROY => {
                if !callbacks.is_null() {
                    drop(Box::from_raw(callbacks));
                    SetWindowLongPtrW(window, GWLP_USERDATA, 0);
                }
                DefWindowProcW(window, message, wparam, lparam)
            }
            _ => DefWindowProcW(window, message, wparam, lparam),
        }
    }
}
impl Tray {
    pub fn start(
        open: impl Fn() + Send + 'static,
        exit: impl Fn() + Send + 'static,
    ) -> Result<Self> {
        let (tx, rx) = std::sync::mpsc::sync_channel(1);
        std::thread::spawn(move || unsafe {
            let instance = GetModuleHandleW(std::ptr::null());
            let class = wide("PokniteTray");
            let mut wc: WNDCLASSW = std::mem::zeroed();
            wc.lpfnWndProc = Some(procedure);
            wc.hInstance = instance;
            wc.lpszClassName = class.as_ptr();
            RegisterClassW(&wc);
            // A hidden top-level window receives Explorer's TaskbarCreated broadcast.
            let window = CreateWindowExW(
                0,
                class.as_ptr(),
                class.as_ptr(),
                0,
                0,
                0,
                0,
                0,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                instance,
                std::ptr::null(),
            );
            if window.is_null() {
                let _ = tx.send(None);
                return;
            }
            let callbacks = Box::new(Callbacks {
                open: Box::new(open),
                exit: Box::new(exit),
            });
            SetWindowLongPtrW(window, GWLP_USERDATA, Box::into_raw(callbacks) as isize);
            if Shell_NotifyIconW(NIM_ADD, &icon(window)) == 0 {
                DestroyWindow(window);
                let _ = tx.send(None);
                return;
            }
            let _ = tx.send(Some(window as usize));
            let mut message: MSG = std::mem::zeroed();
            while GetMessageW(&mut message, std::ptr::null_mut(), 0, 0) > 0 {
                TranslateMessage(&message);
                DispatchMessageW(&message);
            }
        });
        if let Some(window) = rx.recv()? {
            Ok(Self(window))
        } else {
            bail!("Не удалось создать значок Poknite в области уведомлений")
        }
    }
}
