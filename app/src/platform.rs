//! Small OS-specific window helpers.
//!
//! - Windows 11: native rounded corners and shadow (transparent corners showed up white).
//! - Focus juggling for the global shortcut: remember which window was in front, bring Corvox
//!   up, and hand focus back to that window when the shortcut is pressed again.

#[cfg(windows)]
mod imp {
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};
    use windows_sys::Win32::Foundation::HWND;
    use windows_sys::Win32::Graphics::Dwm::{DwmSetWindowAttribute, DWMWA_WINDOW_CORNER_PREFERENCE, DWMWCP_ROUND};
    use windows_sys::Win32::System::Threading::{AttachThreadInput, GetCurrentThreadId};
    use windows_sys::Win32::UI::Input::KeyboardAndMouse::{keybd_event, KEYEVENTF_KEYUP};

    /// A virtual-key code Windows leaves unassigned: no app reacts to it.
    const VK_UNASSIGNED: u8 = 0x88;
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        BringWindowToTop, GetForegroundWindow, GetWindowThreadProcessId, IsIconic, IsWindow, SetForegroundWindow, ShowWindow, SW_RESTORE,
    };

    #[derive(Clone, Copy, PartialEq)]
    pub struct Window(HWND);

    impl Window {
        pub fn from(w: &impl HasWindowHandle) -> Option<Self> {
            match w.window_handle().ok()?.as_raw() {
                RawWindowHandle::Win32(h) => Some(Self(h.hwnd.get() as HWND)),
                _ => None,
            }
        }

        /// The usable area (without the taskbar) of the monitor the window is on, in physical
        /// pixels: (left, top, right, bottom).
        pub fn work_area(&self) -> Option<(i32, i32, i32, i32)> {
            use windows_sys::Win32::Graphics::Gdi::{GetMonitorInfoW, MonitorFromWindow, MONITORINFO, MONITOR_DEFAULTTONEAREST};
            unsafe {
                let mon = MonitorFromWindow(self.0, MONITOR_DEFAULTTONEAREST);
                let mut info: MONITORINFO = std::mem::zeroed();
                info.cbSize = std::mem::size_of::<MONITORINFO>() as u32;
                if GetMonitorInfoW(mon, &mut info) == 0 {
                    return None;
                }
                let r = info.rcWork;
                Some((r.left, r.top, r.right, r.bottom))
            }
        }

        pub fn round_corners(&self) {
            let pref = DWMWCP_ROUND;
            unsafe {
                DwmSetWindowAttribute(self.0, DWMWA_WINDOW_CORNER_PREFERENCE as u32, &pref as *const _ as *const _, std::mem::size_of_val(&pref) as u32);
            }
        }

        /// Watches which window has focus and remembers the last one that wasn't ours, so
        /// the shortcut can go back to it however Corvox got focus (shortcut, click, startup).
        pub fn track_previous(&self) -> std::sync::Arc<std::sync::atomic::AtomicIsize> {
            use std::sync::atomic::{AtomicIsize, Ordering};
            let last = std::sync::Arc::new(AtomicIsize::new(0));
            let (own, out) = (self.0 as isize, last.clone());
            std::thread::spawn(move || loop {
                let fg = unsafe { GetForegroundWindow() } as isize;
                if fg != 0 && fg != own {
                    out.store(fg, Ordering::Relaxed);
                }
                std::thread::sleep(std::time::Duration::from_millis(200));
            });
            last
        }

        pub fn from_raw(raw: isize) -> Option<Self> {
            (raw != 0).then_some(Self(raw as HWND))
        }

        pub fn raw(&self) -> isize {
            self.0 as isize
        }

        /// Sets the big window icon (taskbar button, Alt+Tab). winit only sets the small one.
        pub fn set_big_icon(&self, rgba: &[u8], size: u32) {
            use windows_sys::Win32::Graphics::Gdi::{CreateBitmap, DeleteObject};
            use windows_sys::Win32::UI::WindowsAndMessaging::{CreateIconIndirect, SendMessageW, ICONINFO, ICON_BIG, WM_SETICON};
            let bgra: Vec<u8> = rgba.chunks_exact(4).flat_map(|p| [p[2], p[1], p[0], p[3]]).collect();
            let mask = vec![0u8; (size.div_ceil(16) * 2 * size) as usize]; // 1 bit per pixel, unused: alpha decides
            unsafe {
                let color_bm = CreateBitmap(size as i32, size as i32, 1, 32, bgra.as_ptr() as _);
                let mask_bm = CreateBitmap(size as i32, size as i32, 1, 1, mask.as_ptr() as _);
                let info = ICONINFO { fIcon: 1, xHotspot: 0, yHotspot: 0, hbmMask: mask_bm, hbmColor: color_bm };
                let icon = CreateIconIndirect(&info);
                DeleteObject(color_bm as _);
                DeleteObject(mask_bm as _);
                if !icon.is_null() {
                    SendMessageW(self.0, WM_SETICON, ICON_BIG as usize, icon as isize);
                }
            }
        }

        /// Hides the window completely (no taskbar button), for "minimize to tray".
        pub fn hide(&self) {
            use windows_sys::Win32::UI::WindowsAndMessaging::SW_HIDE;
            unsafe {
                ShowWindow(self.0, SW_HIDE);
            }
        }

        pub fn is_foreground(&self) -> bool {
            unsafe { GetForegroundWindow() == self.0 }
        }

        pub fn exists(&self) -> bool {
            unsafe { IsWindow(self.0) != 0 }
        }

        /// Brings this window to the front. Windows only lets the app that owns the current
        /// foreground window change it, so we briefly share its input queue.
        pub fn focus(&self) -> bool {
            unsafe {
                if IsIconic(self.0) != 0 {
                    ShowWindow(self.0, SW_RESTORE);
                }
                let fg = GetForegroundWindow();
                let fg_thread = if fg.is_null() { 0 } else { GetWindowThreadProcessId(fg, std::ptr::null_mut()) };
                let me = GetCurrentThreadId();
                let attach = fg_thread != 0 && fg_thread != me;
                if attach {
                    AttachThreadInput(me, fg_thread, 1);
                }
                BringWindowToTop(self.0);
                SetForegroundWindow(self.0);
                if attach {
                    AttachThreadInput(me, fg_thread, 0);
                }
                if GetForegroundWindow() != self.0 {
                    // Windows still refused (foreground lock). A synthetic key press counts as
                    // fresh input from this process, which lifts the lock. An unassigned key
                    // code, unlike the usual Alt trick, doesn't open menus in other apps.
                    keybd_event(VK_UNASSIGNED, 0, 0, 0);
                    keybd_event(VK_UNASSIGNED, 0, KEYEVENTF_KEYUP, 0);
                    SetForegroundWindow(self.0);
                }
                GetForegroundWindow() == self.0
            }
        }
    }
}

#[cfg(not(windows))]
mod imp {
    #[derive(Clone, Copy, PartialEq)]
    pub struct Window;

    impl Window {
        pub fn raw(&self) -> isize {
            0
        }
        pub fn hide(&self) {}
        pub fn set_big_icon(&self, _rgba: &[u8], _size: u32) {}
        pub fn from(_w: &impl raw_window_handle::HasWindowHandle) -> Option<Self> {
            None
        }
        pub fn round_corners(&self) {}
        pub fn work_area(&self) -> Option<(i32, i32, i32, i32)> {
            None
        }
        pub fn track_previous(&self) -> std::sync::Arc<std::sync::atomic::AtomicIsize> {
            Default::default()
        }
        pub fn from_raw(_raw: isize) -> Option<Self> {
            None
        }
        pub fn is_foreground(&self) -> bool {
            false
        }
        pub fn exists(&self) -> bool {
            false
        }
        pub fn focus(&self) -> bool {
            false
        }
    }
}

pub use imp::Window;

/// Whether the window `raw` is hidden (minimized to the tray).
#[cfg(windows)]
pub fn is_hidden_raw(raw: isize) -> bool {
    use windows_sys::Win32::UI::WindowsAndMessaging::IsWindowVisible;
    raw != 0 && unsafe { IsWindowVisible(raw as _) } == 0
}

/// Shows a hidden (or minimized) window again and brings it to the front.
#[cfg(windows)]
pub fn show_raw(raw: isize) {
    use windows_sys::Win32::UI::WindowsAndMessaging::{ShowWindow, SW_SHOW};
    if raw == 0 {
        return;
    }
    unsafe {
        ShowWindow(raw as _, SW_SHOW);
    }
    if let Some(w) = Window::from_raw(raw) {
        w.focus();
    }
}

/// Asks the window to close, the same as its close button (works while it's hidden).
#[cfg(windows)]
pub fn close_raw(raw: isize) {
    use windows_sys::Win32::UI::WindowsAndMessaging::{PostMessageW, WM_CLOSE};
    if raw != 0 {
        unsafe {
            PostMessageW(raw as _, WM_CLOSE, 0, 0);
        }
    }
}

#[cfg(not(windows))]
pub fn is_hidden_raw(_raw: isize) -> bool {
    false
}

#[cfg(not(windows))]
pub fn show_raw(_raw: isize) {}

#[cfg(not(windows))]
pub fn close_raw(_raw: isize) {}

/// True when the OS draws rounded corners for us (Windows 11), so we paint square ones.
pub fn native_corners() -> bool {
    cfg!(windows)
}

/// Waits (up to `ms`) for the process `pid` to exit. Used after an update, so the new version
/// opens once the old one has let go of the shortcut and the audio devices.
#[cfg(windows)]
pub fn wait_for_exit(pid: u32, ms: u32) {
    use windows_sys::Win32::Foundation::CloseHandle;
    use windows_sys::Win32::System::Threading::{OpenProcess, WaitForSingleObject, PROCESS_SYNCHRONIZE};
    unsafe {
        let h = OpenProcess(PROCESS_SYNCHRONIZE, 0, pid);
        if !h.is_null() {
            WaitForSingleObject(h, ms);
            CloseHandle(h);
        }
    }
}

#[cfg(not(windows))]
pub fn wait_for_exit(_pid: u32, ms: u32) {
    std::thread::sleep(std::time::Duration::from_millis(ms.min(1500) as u64));
}
