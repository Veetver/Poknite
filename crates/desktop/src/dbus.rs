//! Minimal native libdbus binding: only the freedesktop notification method.
//! The iterator layout matches the public DBusMessageIter ABI in dbus-message.h. Connections
//! and replies have independent references and are released on every path.
use anyhow::{Result, bail};
use std::{
    ffi::{CString, c_int, c_uint, c_void},
    ptr,
};
#[repr(C)]
pub(super) struct Iter {
    dummy1: *mut c_void,
    dummy2: *mut c_void,
    dummy3: u32,
    dummy4: c_int,
    dummy5: c_int,
    dummy6: c_int,
    dummy7: c_int,
    dummy8: c_int,
    dummy9: c_int,
    dummy10: c_int,
    dummy11: c_int,
    pad1: c_int,
    pad2: *mut c_void,
    pad3: *mut c_void,
}
impl Iter {
    pub(super) fn new() -> Self {
        unsafe { std::mem::zeroed() }
    }
}
#[link(name = "dbus-1")]
unsafe extern "C" {
    fn dbus_threads_init_default() -> c_int;
    fn dbus_bus_get(kind: c_int, error: *mut c_void) -> *mut c_void;
    fn dbus_connection_unref(connection: *mut c_void);
    fn dbus_message_unref(message: *mut c_void);
    fn dbus_message_new_method_call(
        destination: *const i8,
        path: *const i8,
        interface: *const i8,
        method: *const i8,
    ) -> *mut c_void;
    fn dbus_message_iter_init_append(message: *mut c_void, iter: *mut Iter);
    fn dbus_message_iter_append_basic(iter: *mut Iter, kind: c_int, value: *const c_void) -> c_int;
    fn dbus_message_iter_open_container(
        iter: *mut Iter,
        kind: c_int,
        signature: *const i8,
        sub: *mut Iter,
    ) -> c_int;
    fn dbus_message_iter_close_container(iter: *mut Iter, sub: *mut Iter) -> c_int;
    fn dbus_connection_send_with_reply_and_block(
        connection: *mut c_void,
        message: *mut c_void,
        timeout: c_int,
        error: *mut c_void,
    ) -> *mut c_void;
    fn dbus_message_get_args(message: *mut c_void, error: *mut c_void, first: c_int, ...) -> c_int;
}
struct Message(*mut c_void);
impl Drop for Message {
    fn drop(&mut self) {
        unsafe {
            if !self.0.is_null() {
                dbus_message_unref(self.0)
            }
        }
    }
}
struct Connection(*mut c_void);
impl Drop for Connection {
    fn drop(&mut self) {
        unsafe {
            if !self.0.is_null() {
                dbus_connection_unref(self.0)
            }
        }
    }
}
pub(super) fn initialize() {
    static INITIALIZE: std::sync::Once = std::sync::Once::new();
    INITIALIZE.call_once(|| unsafe {
        dbus_threads_init_default();
    });
}
pub fn notify(title: &str, body: &str, replaces: u32, quiet: bool) -> Result<u32> {
    initialize();
    let body = body
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;");
    let values = [
        CString::new("Poknite")?,
        CString::new("dialog-information")?,
        CString::new(title)?,
        CString::new(body)?,
    ];
    unsafe {
        let connection = Connection(dbus_bus_get(0, ptr::null_mut()));
        if connection.0.is_null() {
            bail!("Шина D-Bus недоступна")
        }
        let message = Message(dbus_message_new_method_call(
            c"org.freedesktop.Notifications".as_ptr(),
            c"/org/freedesktop/Notifications".as_ptr(),
            c"org.freedesktop.Notifications".as_ptr(),
            c"Notify".as_ptr(),
        ));
        if message.0.is_null() {
            bail!("Не хватает памяти для уведомления")
        }
        let mut iter = Iter::new();
        dbus_message_iter_init_append(message.0, &mut iter);
        let append = |iter: &mut Iter, kind: u8, value: *const c_void| -> Result<()> {
            if dbus_message_iter_append_basic(iter, kind as c_int, value) == 0 {
                bail!("Не удалось создать уведомление")
            };
            Ok(())
        };
        let string = |iter: &mut Iter, value: &CString| -> Result<()> {
            let pointer = value.as_ptr();
            append(iter, b's', (&pointer as *const *const i8).cast())
        };
        string(&mut iter, &values[0])?;
        append(&mut iter, b'u', (&replaces as *const u32).cast())?;
        for value in &values[1..] {
            string(&mut iter, value)?;
        }
        let mut array = Iter::new();
        if dbus_message_iter_open_container(&mut iter, b'a' as c_int, c"s".as_ptr(), &mut array)
            == 0
            || dbus_message_iter_close_container(&mut iter, &mut array) == 0
        {
            bail!("Не удалось создать уведомление")
        }
        let mut hints = Iter::new();
        if dbus_message_iter_open_container(&mut iter, b'a' as c_int, c"{sv}".as_ptr(), &mut hints)
            == 0
        {
            bail!("Не удалось создать уведомление")
        }
        if quiet {
            let mut entry = Iter::new();
            if dbus_message_iter_open_container(&mut hints, b'e' as c_int, ptr::null(), &mut entry)
                == 0
            {
                bail!("Не удалось создать уведомление")
            }
            string(&mut entry, &CString::new("suppress-sound")?)?;
            let mut variant = Iter::new();
            if dbus_message_iter_open_container(
                &mut entry,
                b'v' as c_int,
                c"b".as_ptr(),
                &mut variant,
            ) == 0
            {
                bail!("Не удалось создать уведомление")
            }
            let value = 1u32;
            append(&mut variant, b'b', (&value as *const u32).cast())?;
            if dbus_message_iter_close_container(&mut entry, &mut variant) == 0
                || dbus_message_iter_close_container(&mut hints, &mut entry) == 0
            {
                bail!("Не удалось создать уведомление")
            }
        }
        if dbus_message_iter_close_container(&mut iter, &mut hints) == 0 {
            bail!("Не удалось создать уведомление")
        }
        let timeout = -1i32;
        append(&mut iter, b'i', (&timeout as *const i32).cast())?;
        let reply = Message(dbus_connection_send_with_reply_and_block(
            connection.0,
            message.0,
            3000,
            ptr::null_mut(),
        ));
        if reply.0.is_null() {
            bail!("Служба системных уведомлений недоступна")
        }
        let mut id = 0u32;
        if dbus_message_get_args(
            reply.0,
            ptr::null_mut(),
            b'u' as c_int,
            &mut id as *mut c_uint,
            0 as c_int,
        ) == 0
        {
            bail!("Некорректный ответ службы уведомлений")
        };
        Ok(id)
    }
}

pub fn close_notification(id: u32) -> Result<()> {
    initialize();
    unsafe {
        let connection = Connection(dbus_bus_get(0, ptr::null_mut()));
        if connection.0.is_null() {
            bail!("Шина D-Bus недоступна");
        }
        let message = Message(dbus_message_new_method_call(
            c"org.freedesktop.Notifications".as_ptr(),
            c"/org/freedesktop/Notifications".as_ptr(),
            c"org.freedesktop.Notifications".as_ptr(),
            c"CloseNotification".as_ptr(),
        ));
        if message.0.is_null() {
            bail!("Не хватает памяти для отмены уведомления");
        }
        let mut iter = Iter::new();
        dbus_message_iter_init_append(message.0, &mut iter);
        if dbus_message_iter_append_basic(&mut iter, b'u' as c_int, (&id as *const u32).cast()) == 0
        {
            bail!("Не удалось отменить уведомление");
        }
        let reply = Message(dbus_connection_send_with_reply_and_block(
            connection.0,
            message.0,
            3000,
            ptr::null_mut(),
        ));
        if reply.0.is_null() {
            bail!("Служба уведомлений недоступна");
        }
        Ok(())
    }
}
#[cfg(test)]
mod tests {
    #[test]
    #[ignore = "requires an isolated org.freedesktop.Notifications fixture"]
    fn native_method_and_replacement() {
        let id = super::notify("Русский заголовок", "<текст>&", 0, true).unwrap();
        assert_eq!(id, 42);
        assert_eq!(
            super::notify("Русский заголовок", "<текст>&", id, true).unwrap(),
            42
        );
    }
}
