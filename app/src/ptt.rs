//! Holds your Discord push-to-talk key while the voice is speaking.

use enigo::{Direction, Enigo, Key, Keyboard, Settings};

pub const KEYS: [&str; 15] = [
    "F13", "F14", "F15", "F16", "F17", "F18", "F19", "F20", "F21", "F22", "F23", "F24", "ScrollLock", "Pause", "CapsLock",
];

fn parse(name: &str) -> Option<Key> {
    Some(match name {
        "F13" => Key::F13,
        "F14" => Key::F14,
        "F15" => Key::F15,
        "F16" => Key::F16,
        "F17" => Key::F17,
        "F18" => Key::F18,
        "F19" => Key::F19,
        "F20" => Key::F20,
        "F21" => Key::F21,
        "F22" => Key::F22,
        "F23" => Key::F23,
        "F24" => Key::F24,
        #[cfg(target_os = "windows")]
        "ScrollLock" => Key::Scroll,
        #[cfg(all(unix, not(target_os = "macos")))]
        "ScrollLock" => Key::ScrollLock,
        #[cfg(not(target_os = "macos"))]
        "Pause" => Key::Pause,
        "CapsLock" => Key::CapsLock,
        s if s.chars().count() == 1 => Key::Unicode(s.chars().next()?.to_ascii_lowercase()),
        _ => return None,
    })
}

pub struct Ptt {
    enigo: Option<Enigo>,
    held: Option<Key>,
}

impl Ptt {
    pub fn new() -> Self {
        Self { enigo: Enigo::new(&Settings::default()).ok(), held: None }
    }

    pub fn press(&mut self, name: Option<&str>) {
        if self.held.is_some() {
            return;
        }
        let (Some(enigo), Some(key)) = (self.enigo.as_mut(), name.and_then(parse)) else { return };
        if enigo.key(key, Direction::Press).is_ok() {
            self.held = Some(key);
        }
    }

    pub fn release(&mut self) {
        if let (Some(enigo), Some(key)) = (self.enigo.as_mut(), self.held.take()) {
            let _ = enigo.key(key, Direction::Release);
        }
    }

    pub fn is_held(&self) -> bool {
        self.held.is_some()
    }
}

impl Drop for Ptt {
    fn drop(&mut self) {
        self.release();
    }
}
