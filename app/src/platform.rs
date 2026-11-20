//! Small OS-specific window helpers.
//!
//! On Windows 11 we ask for native rounded corners and a shadow: the window's transparent
//! corners showed up white on screen, so we don't rely on transparency there.

#[cfg(windows)]
mod imp {
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};
    use windows_sys::Win32::Foundation::HWND;
    use windows_sys::Win32::Graphics::Dwm::{DwmSetWindowAttribute, DWMWA_WINDOW_CORNER_PREFERENCE, DWMWCP_ROUND};
    
    #[derive(Clone, Copy)]
    pub struct Window(HWND);

    impl Window {
        pub fn from(w: &impl HasWindowHandle) -> Option<Self> {
            match w.window_handle().ok()?.as_raw() {
                RawWindowHandle::Win32(h) => Some(Self(h.hwnd.get() as HWND)),
                _ => None,
            }
        }

        pub fn round_corners(&self) {
            let pref = DWMWCP_ROUND;
            unsafe {
                DwmSetWindowAttribute(self.0, DWMWA_WINDOW_CORNER_PREFERENCE as u32, &pref as *const _ as *const _, std::mem::size_of_val(&pref) as u32);
            }
        }
    }
}

#[cfg(not(windows))]
mod imp {
    #[derive(Clone, Copy)]
    pub struct Window;

    impl Window {
        pub fn from(_w: &impl raw_window_handle::HasWindowHandle) -> Option<Self> {
            None
        }
        pub fn round_corners(&self) {}
    }
}

pub use imp::Window;

/// True when the OS draws rounded corners for us (Windows 11), so we paint square ones.
pub fn native_corners() -> bool {
    cfg!(windows)
}
