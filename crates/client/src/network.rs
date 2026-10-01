//! OS events wake the reconnect loop immediately when network interfaces change.
use anyhow::Result;
use tokio::sync::watch;
#[cfg(target_os = "linux")]
pub struct Monitor;
#[cfg(target_os = "linux")]
impl Monitor {
    pub fn start(restart: watch::Sender<u64>) -> Result<Self> {
        use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
        let raw = unsafe {
            libc::socket(
                libc::AF_NETLINK,
                libc::SOCK_RAW | libc::SOCK_CLOEXEC,
                libc::NETLINK_ROUTE,
            )
        };
        if raw < 0 {
            return Err(std::io::Error::last_os_error().into());
        }
        let socket = unsafe { OwnedFd::from_raw_fd(raw) };
        let mut address: libc::sockaddr_nl = unsafe { std::mem::zeroed() };
        address.nl_family = libc::AF_NETLINK as u16;
        address.nl_groups = 1 | 0x10 | 0x100 | 0x40 | 0x400;
        if unsafe {
            libc::bind(
                socket.as_raw_fd(),
                (&address as *const libc::sockaddr_nl).cast(),
                std::mem::size_of_val(&address) as u32,
            )
        } < 0
        {
            return Err(std::io::Error::last_os_error().into());
        }
        std::thread::Builder::new()
            .name("poknite-network".into())
            .stack_size(256 * 1024)
            .spawn(move || {
                let mut buffer = [0u8; 4096];
                loop {
                    let size = unsafe {
                        libc::recv(
                            socket.as_raw_fd(),
                            buffer.as_mut_ptr().cast(),
                            buffer.len(),
                            0,
                        )
                    };
                    if size > 0 {
                        restart.send_modify(|v| *v = v.wrapping_add(1));
                    } else if std::io::Error::last_os_error().kind()
                        != std::io::ErrorKind::Interrupted
                    {
                        break;
                    }
                }
            })?;
        Ok(Self)
    }
}
#[cfg(windows)]
pub struct Monitor {
    handle: windows_sys::Win32::Foundation::HANDLE,
    context: *mut watch::Sender<u64>,
}
#[cfg(windows)]
impl Monitor {
    pub fn start(restart: watch::Sender<u64>) -> Result<Self> {
        use windows_sys::Win32::NetworkManagement::IpHelper::*;
        unsafe extern "system" fn changed(
            context: *const std::ffi::c_void,
            _row: *const MIB_IPINTERFACE_ROW,
            _kind: MIB_NOTIFICATION_TYPE,
        ) {
            unsafe {
                (*(context as *const watch::Sender<u64>)).send_modify(|n| *n = n.wrapping_add(1));
            }
        }
        let context = Box::into_raw(Box::new(restart));
        let mut handle = std::ptr::null_mut();
        let error = unsafe {
            NotifyIpInterfaceChange(0, Some(changed), context.cast(), false, &mut handle)
        };
        if error != 0 {
            unsafe { drop(Box::from_raw(context)) };
            return Err(std::io::Error::from_raw_os_error(error as i32).into());
        }
        Ok(Self { handle, context })
    }
}
#[cfg(windows)]
impl Drop for Monitor {
    fn drop(&mut self) {
        unsafe {
            windows_sys::Win32::NetworkManagement::IpHelper::CancelMibChangeNotify2(self.handle);
            drop(Box::from_raw(self.context));
        }
    }
}
#[cfg(not(any(windows, target_os = "linux")))]
pub struct Monitor;
#[cfg(not(any(windows, target_os = "linux")))]
impl Monitor {
    pub fn start(_: watch::Sender<u64>) -> Result<Self> {
        Ok(Self)
    }
}
