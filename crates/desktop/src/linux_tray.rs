//! Native StatusNotifierItem. A private bus connection runs on the existing
//! Tokio reactor; idle trays have no timer or dedicated polling thread.
use crate::dbus::Iter;
use anyhow::{Result, bail};
use std::{
    ffi::{CStr, CString, c_int, c_void},
    os::fd::AsRawFd,
    ptr,
};
use tokio::{io::unix::AsyncFd, runtime::Handle, task::JoinHandle};
#[link(name = "dbus-1")]
unsafe extern "C" {
    fn dbus_bus_get_private(kind: c_int, error: *mut c_void) -> *mut c_void;
    fn dbus_bus_request_name(
        connection: *mut c_void,
        name: *const i8,
        flags: u32,
        error: *mut c_void,
    ) -> c_int;
    fn dbus_connection_set_exit_on_disconnect(connection: *mut c_void, enabled: c_int);
    fn dbus_connection_close(connection: *mut c_void);
    fn dbus_connection_unref(connection: *mut c_void);
    fn dbus_connection_set_max_message_size(connection: *mut c_void, size: std::ffi::c_long);
    fn dbus_connection_set_max_received_size(connection: *mut c_void, size: std::ffi::c_long);
    fn dbus_connection_get_unix_fd(connection: *mut c_void, fd: *mut c_int) -> c_int;
    fn dbus_connection_get_is_connected(connection: *mut c_void) -> c_int;
    fn dbus_connection_get_outgoing_size(connection: *mut c_void) -> std::ffi::c_long;
    fn dbus_connection_read_write(connection: *mut c_void, timeout: c_int) -> c_int;
    fn dbus_connection_pop_message(connection: *mut c_void) -> *mut c_void;
    fn dbus_message_get_type(message: *mut c_void) -> c_int;
    fn dbus_message_get_path(message: *mut c_void) -> *const i8;
    fn dbus_message_get_interface(message: *mut c_void) -> *const i8;
    fn dbus_message_get_member(message: *mut c_void) -> *const i8;
    fn dbus_message_unref(message: *mut c_void);
    fn dbus_message_new_method_call(
        destination: *const i8,
        path: *const i8,
        interface: *const i8,
        method: *const i8,
    ) -> *mut c_void;
    fn dbus_message_new_method_return(message: *mut c_void) -> *mut c_void;
    fn dbus_message_new_error(
        message: *mut c_void,
        name: *const i8,
        text: *const i8,
    ) -> *mut c_void;
    fn dbus_connection_send_with_reply_and_block(
        connection: *mut c_void,
        message: *mut c_void,
        timeout: c_int,
        error: *mut c_void,
    ) -> *mut c_void;
    fn dbus_connection_send(
        connection: *mut c_void,
        message: *mut c_void,
        serial: *mut u32,
    ) -> c_int;
    fn dbus_message_iter_init(message: *mut c_void, iter: *mut Iter) -> c_int;
    fn dbus_message_iter_next(iter: *mut Iter) -> c_int;
    fn dbus_message_iter_get_arg_type(iter: *mut Iter) -> c_int;
    fn dbus_message_iter_get_basic(iter: *mut Iter, value: *mut c_void);
    fn dbus_message_iter_init_append(message: *mut c_void, iter: *mut Iter);
    fn dbus_message_iter_append_basic(iter: *mut Iter, kind: c_int, value: *const c_void) -> c_int;
    fn dbus_message_iter_open_container(
        iter: *mut Iter,
        kind: c_int,
        signature: *const i8,
        sub: *mut Iter,
    ) -> c_int;
    fn dbus_message_iter_close_container(iter: *mut Iter, sub: *mut Iter) -> c_int;
}
struct Connection(usize);
impl Connection {
    fn pointer(&self) -> *mut c_void {
        self.0 as *mut c_void
    }
}
impl Drop for Connection {
    fn drop(&mut self) {
        unsafe {
            dbus_connection_close(self.pointer());
            dbus_connection_unref(self.pointer());
        }
    }
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
struct Fd(c_int);
impl AsRawFd for Fd {
    fn as_raw_fd(&self) -> c_int {
        self.0
    }
}
pub struct Tray {
    task: JoinHandle<()>,
}
impl Drop for Tray {
    fn drop(&mut self) {
        self.task.abort();
    }
}
fn string(iter: &mut Iter, text: &str) -> Result<()> {
    let value = CString::new(text)?;
    let pointer = value.as_ptr();
    basic(iter, b's', (&pointer as *const *const i8).cast())
}
fn object_path(iter: &mut Iter, text: &str) -> Result<()> {
    let value = CString::new(text)?;
    let pointer = value.as_ptr();
    basic(iter, b'o', (&pointer as *const *const i8).cast())
}
fn basic(iter: &mut Iter, kind: u8, value: *const c_void) -> Result<()> {
    if unsafe { dbus_message_iter_append_basic(iter, kind as c_int, value) } == 0 {
        bail!("Не хватает памяти для значка")
    };
    Ok(())
}
fn container(
    iter: &mut Iter,
    kind: u8,
    signature: Option<&CStr>,
    f: impl FnOnce(&mut Iter) -> Result<()>,
) -> Result<()> {
    let mut child = Iter::new();
    if unsafe {
        dbus_message_iter_open_container(
            iter,
            kind as c_int,
            signature.map_or(ptr::null(), CStr::as_ptr),
            &mut child,
        )
    } == 0
    {
        bail!("Не хватает памяти для значка")
    };
    f(&mut child)?;
    if unsafe { dbus_message_iter_close_container(iter, &mut child) } == 0 {
        bail!("Не хватает памяти для значка")
    };
    Ok(())
}
fn incoming_string(iter: &mut Iter) -> Option<String> {
    if unsafe { dbus_message_iter_get_arg_type(iter) } != b's' as c_int {
        return None;
    };
    let mut pointer: *const i8 = ptr::null();
    unsafe {
        dbus_message_iter_get_basic(iter, (&mut pointer as *mut *const i8).cast());
    }
    if pointer.is_null() {
        None
    } else {
        Some(
            unsafe { CStr::from_ptr(pointer) }
                .to_string_lossy()
                .into_owned(),
        )
    }
}
fn borrowed_text<'a>(pointer: *const i8) -> &'a str {
    if pointer.is_null() {
        ""
    } else {
        unsafe { CStr::from_ptr(pointer) }.to_str().unwrap_or("")
    }
}
static PROPERTIES: &[(&str, &CStr)] = &[
    ("Category", c"s"),
    ("Id", c"s"),
    ("Title", c"s"),
    ("Status", c"s"),
    ("WindowId", c"u"),
    ("IconName", c"s"),
    ("IconPixmap", c"a(iiay)"),
    ("OverlayIconName", c"s"),
    ("OverlayIconPixmap", c"a(iiay)"),
    ("AttentionIconName", c"s"),
    ("AttentionIconPixmap", c"a(iiay)"),
    ("AttentionMovieName", c"s"),
    ("ToolTip", c"(sa(iiay)ss)"),
    ("ItemIsMenu", c"b"),
    ("Menu", c"o"),
];
fn property(iter: &mut Iter, name: &str) -> Result<()> {
    match name {
        "Category" => string(iter, "Communications"),
        "Id" | "Title" => string(iter, "Poknite"),
        "Status" => string(iter, "Active"),
        "IconName" => string(iter, "dialog-information"),
        "OverlayIconName" | "AttentionIconName" | "AttentionMovieName" => string(iter, ""),
        "WindowId" => basic(iter, b'u', (&0u32 as *const u32).cast()),
        "ItemIsMenu" => basic(iter, b'b', (&0u32 as *const u32).cast()),
        "Menu" => object_path(iter, "/"),
        "IconPixmap" | "OverlayIconPixmap" | "AttentionIconPixmap" => {
            container(iter, b'a', Some(c"(iiay)"), |_| Ok(()))
        }
        "ToolTip" => container(iter, b'r', None, |child| {
            string(child, "dialog-information")?;
            container(child, b'a', Some(c"(iiay)"), |_| Ok(()))?;
            string(child, "Poknite")?;
            string(child, "Открыть сообщения")
        }),
        _ => bail!("Неизвестное свойство"),
    }
}
fn call(
    connection: &Connection,
    destination: &CStr,
    path: &CStr,
    interface: &CStr,
    method: &CStr,
    arg: Option<&str>,
) -> Result<Message> {
    let message = Message(unsafe {
        dbus_message_new_method_call(
            destination.as_ptr(),
            path.as_ptr(),
            interface.as_ptr(),
            method.as_ptr(),
        )
    });
    if message.0.is_null() {
        bail!("Не хватает памяти для значка")
    }
    if let Some(arg) = arg {
        let mut iter = Iter::new();
        unsafe {
            dbus_message_iter_init_append(message.0, &mut iter);
        }
        string(&mut iter, arg)?;
    }
    let reply = Message(unsafe {
        dbus_connection_send_with_reply_and_block(
            connection.pointer(),
            message.0,
            1000,
            ptr::null_mut(),
        )
    });
    if reply.0.is_null() || unsafe { dbus_message_get_type(reply.0) } == 3 {
        bail!("Хост значков недоступен")
    };
    Ok(reply)
}
fn register(connection: &Connection, name: &str) -> Result<()> {
    let _ = call(
        connection,
        c"org.kde.StatusNotifierWatcher",
        c"/StatusNotifierWatcher",
        c"org.kde.StatusNotifierWatcher",
        c"RegisterStatusNotifierItem",
        Some(name),
    )?;
    Ok(())
}
fn handle(
    connection: &Connection,
    message: &Message,
    open: &impl Fn(),
    context: &impl Fn(),
) -> Result<()> {
    if unsafe { dbus_message_get_type(message.0) } != 1 {
        return Ok(());
    }
    let path = borrowed_text(unsafe { dbus_message_get_path(message.0) });
    let interface = borrowed_text(unsafe { dbus_message_get_interface(message.0) });
    let member = borrowed_text(unsafe { dbus_message_get_member(message.0) });
    let response = Message(unsafe { dbus_message_new_method_return(message.0) });
    if response.0.is_null() {
        bail!("Не хватает памяти для значка")
    }
    let mut output = Iter::new();
    unsafe {
        dbus_message_iter_init_append(response.0, &mut output);
    }
    let mut input = Iter::new();
    unsafe {
        dbus_message_iter_init(message.0, &mut input);
    }
    let mut known = true;
    match (path, interface, member) {
        (
            "/StatusNotifierItem",
            "org.kde.StatusNotifierItem" | "org.freedesktop.StatusNotifierItem",
            "Activate" | "SecondaryActivate",
        ) => open(),
        (
            "/StatusNotifierItem",
            "org.kde.StatusNotifierItem" | "org.freedesktop.StatusNotifierItem",
            "ContextMenu",
        ) => context(),
        (
            "/StatusNotifierItem",
            "org.kde.StatusNotifierItem" | "org.freedesktop.StatusNotifierItem",
            "Scroll",
        ) => {}
        ("/StatusNotifierItem", "org.freedesktop.DBus.Properties", "GetAll") => {
            container(&mut output, b'a', Some(c"{sv}"), |array| {
                for (name, signature) in PROPERTIES {
                    container(array, b'e', None, |entry| {
                        string(entry, name)?;
                        container(entry, b'v', Some(signature), |variant| {
                            property(variant, name)
                        })
                    })?;
                }
                Ok(())
            })?;
        }
        ("/StatusNotifierItem", "org.freedesktop.DBus.Properties", "Get") => {
            let _interface = incoming_string(&mut input);
            unsafe {
                dbus_message_iter_next(&mut input);
            }
            let name = incoming_string(&mut input).unwrap_or_default();
            if let Some((_, signature)) = PROPERTIES.iter().find(|(n, _)| *n == name) {
                container(&mut output, b'v', Some(signature), |variant| {
                    property(variant, &name)
                })?;
            } else {
                known = false
            }
        }
        ("/StatusNotifierItem", "org.freedesktop.DBus.Introspectable", "Introspect") => {
            string(&mut output, INTROSPECTION)?
        }
        (_, "org.freedesktop.DBus.Peer", "Ping") => {}
        _ => known = false,
    }
    let error;
    let reply = if known {
        &response
    } else {
        error = Message(unsafe {
            dbus_message_new_error(
                message.0,
                c"org.freedesktop.DBus.Error.UnknownMethod".as_ptr(),
                c"Unknown method or property".as_ptr(),
            )
        });
        if error.0.is_null() {
            bail!("Не хватает памяти для значка")
        };
        &error
    };
    if unsafe { dbus_connection_send(connection.pointer(), reply.0, ptr::null_mut()) } == 0 {
        bail!("Не хватает памяти для значка")
    };
    Ok(())
}
impl Tray {
    pub fn start(
        runtime: Handle,
        open: impl Fn() + Send + 'static,
        context: impl Fn() + Send + 'static,
    ) -> Result<Option<Self>> {
        crate::dbus::initialize();
        let raw = unsafe { dbus_bus_get_private(0, ptr::null_mut()) };
        if raw.is_null() {
            return Ok(None);
        }
        let connection = Connection(raw as usize);
        unsafe {
            dbus_connection_set_exit_on_disconnect(raw, 0);
            dbus_connection_set_max_message_size(raw, 32768);
            dbus_connection_set_max_received_size(raw, 131072);
        }
        let reply = match call(
            &connection,
            c"org.freedesktop.DBus",
            c"/org/freedesktop/DBus",
            c"org.freedesktop.DBus",
            c"NameHasOwner",
            Some("org.kde.StatusNotifierWatcher"),
        ) {
            Ok(reply) => reply,
            Err(_) => return Ok(None),
        };
        let mut input = Iter::new();
        unsafe {
            dbus_message_iter_init(reply.0, &mut input);
        }
        let mut has_owner = 0u32;
        if unsafe { dbus_message_iter_get_arg_type(&mut input) } != b'b' as c_int {
            return Ok(None);
        };
        unsafe {
            dbus_message_iter_get_basic(&mut input, (&mut has_owner as *mut u32).cast());
        }
        if has_owner == 0 {
            return Ok(None);
        }
        let name = format!("org.kde.StatusNotifierItem-{}-1", std::process::id());
        let service = CString::new(&*name)?;
        if unsafe { dbus_bus_request_name(raw, service.as_ptr(), 4, ptr::null_mut()) } != 1 {
            return Ok(None);
        }
        if register(&connection, &name).is_err() {
            return Ok(None);
        }
        let mut fd = 0;
        if unsafe { dbus_connection_get_unix_fd(raw, &mut fd) } == 0 {
            return Ok(None);
        }
        let task=runtime.spawn(async move{
            let Ok(socket)=AsyncFd::new(Fd(fd))else{return};
            loop {
                unsafe{dbus_connection_read_write(connection.pointer(),0);}
                if unsafe{dbus_connection_get_is_connected(connection.pointer())}==0{break}
                let mut processed=0;
                for _ in 0..32 {let message=Message(unsafe{dbus_connection_pop_message(connection.pointer())});if message.0.is_null(){break}processed+=1;if handle(&connection,&message,&open,&context).is_err(){return}}
                if unsafe{dbus_connection_get_outgoing_size(connection.pointer())}>65536{break}
                if processed==32{tokio::task::yield_now().await;continue}
                if unsafe{dbus_connection_get_outgoing_size(connection.pointer())}>0 {
                    tokio::select!{read=socket.readable()=>{if let Ok(mut guard)=read{guard.clear_ready()}else{break}},write=socket.writable()=>{if let Ok(mut guard)=write{guard.clear_ready()}else{break}}}
                }else{match socket.readable().await{Ok(mut ready)=>ready.clear_ready(),Err(_)=>break}}
            }
        });
        Ok(Some(Self { task }))
    }
}
const INTROSPECTION: &str = r#"<node><interface name="org.kde.StatusNotifierItem"><property name="Category" type="s" access="read"/><property name="Id" type="s" access="read"/><property name="Title" type="s" access="read"/><property name="Status" type="s" access="read"/><property name="WindowId" type="u" access="read"/><property name="IconName" type="s" access="read"/><property name="IconPixmap" type="a(iiay)" access="read"/><property name="ToolTip" type="(sa(iiay)ss)" access="read"/><property name="ItemIsMenu" type="b" access="read"/><property name="Menu" type="o" access="read"/><method name="Activate"><arg type="i" direction="in"/><arg type="i" direction="in"/></method><method name="SecondaryActivate"><arg type="i" direction="in"/><arg type="i" direction="in"/></method><method name="ContextMenu"><arg type="i" direction="in"/><arg type="i" direction="in"/></method><method name="Scroll"><arg type="i" direction="in"/><arg type="s" direction="in"/></method></interface><interface name="org.freedesktop.DBus.Properties"><method name="Get"><arg type="s" direction="in"/><arg type="s" direction="in"/><arg type="v" direction="out"/></method><method name="GetAll"><arg type="s" direction="in"/><arg type="a{sv}" direction="out"/></method></interface><interface name="org.freedesktop.DBus.Introspectable"><method name="Introspect"><arg type="s" direction="out"/></method></interface></node>"#;

#[cfg(test)]
mod tests {
    #[tokio::test]
    #[ignore = "requires an isolated StatusNotifierWatcher fixture"]
    async fn tray_registration_properties_activation_and_cleanup() {
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let open = tx.clone();
        let tray = super::Tray::start(
            tokio::runtime::Handle::current(),
            move || {
                let _ = open.send("open");
            },
            move || {
                let _ = tx.send("context");
            },
        )
        .unwrap()
        .expect("fixture watcher");
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            assert_eq!(rx.recv().await, Some("open"));
            assert_eq!(rx.recv().await, Some("context"));
        })
        .await
        .unwrap();
        drop(tray);
        tokio::task::yield_now().await;
    }
    #[tokio::test]
    #[ignore = "requires an isolated bus without a StatusNotifierWatcher"]
    async fn absent_tray_watcher_is_optional() {
        assert!(
            super::Tray::start(tokio::runtime::Handle::current(), || {}, || {})
                .unwrap()
                .is_none()
        );
    }
}
