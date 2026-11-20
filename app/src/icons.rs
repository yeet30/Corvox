//! Small line icons, drawn for Corvox (24×24, white strokes, tinted at draw time).

use eframe::egui::{self, Color32, ImageSource, Response, Ui, Vec2};

macro_rules! svg {
    ($body:expr) => {
        concat!(
            r#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24" width="48" height="48" fill="none" stroke="white" stroke-width="2" stroke-linecap="round" stroke-linejoin="round">"#,
            $body,
            "</svg>"
        )
    };
}

#[derive(Clone, Copy)]
pub enum Icon {
    Mic,
    Sliders,
    ChevronDown,
    ChevronUp,
    Pin,
    PinOff,
    Close,
    Play,
    Stop,
    User,
    Cpu,
    Download,
    Gauge,
    Plus,
    Trash,
    Back,
    Refresh,
    Check,
    Send,
    Volume,
    VolumeOff,
    Pause,
}

impl Icon {
    fn svg(self) -> (&'static str, &'static [u8]) {
        let (name, s): (&str, &str) = match self {
            Icon::Mic => ("mic", svg!(r#"<rect x="9" y="2" width="6" height="12" rx="3"/><path d="M5 11a7 7 0 0 0 14 0"/><path d="M12 18v4"/>"#)),
            Icon::Sliders => ("sliders", svg!(r#"<path d="M4 6h9M17 6h3M4 12h3M11 12h9M4 18h11M19 18h1"/><circle cx="15" cy="6" r="2"/><circle cx="9" cy="12" r="2"/><circle cx="17" cy="18" r="2"/>"#)),
            Icon::ChevronDown => ("chev-down", svg!(r#"<path d="M6 9l6 6 6-6"/>"#)),
            Icon::ChevronUp => ("chev-up", svg!(r#"<path d="M6 15l6-6 6 6"/>"#)),
            Icon::Pin => ("pin", svg!(r#"<path d="M12 16v6"/><path d="M8 3h8l-1.5 6 3.5 3.5V16H6v-3.5L9.5 9z"/>"#)),
            Icon::PinOff => ("pin-off", svg!(r#"<path d="M12 16v6"/><path d="M8 3h8l-1.5 6 3.5 3.5V16H6v-3.5L9.5 9z" opacity="0.5"/><path d="M3 3l18 18"/>"#)),
            Icon::Close => ("close", svg!(r#"<path d="M6 6l12 12M18 6L6 18"/>"#)),
            Icon::Play => ("play", svg!(r#"<path d="M8 5v14l11-7z" fill="white"/>"#)),
            Icon::Stop => ("stop", svg!(r#"<rect x="6" y="6" width="12" height="12" rx="2" fill="white"/>"#)),
            Icon::User => ("user", svg!(r#"<circle cx="12" cy="8" r="4"/><path d="M4 21c0-4 3.6-6 8-6s8 2 8 6"/>"#)),
            Icon::Cpu => ("cpu", svg!(r#"<rect x="6" y="6" width="12" height="12" rx="2"/><path d="M10 10h4v4h-4zM9 2v4M15 2v4M9 18v4M15 18v4M2 9h4M2 15h4M18 9h4M18 15h4"/>"#)),
            Icon::Download => ("download", svg!(r#"<path d="M12 3v12M7 10l5 5 5-5M5 21h14"/>"#)),
            Icon::Gauge => ("gauge", svg!(r#"<path d="M4 18a9 9 0 1 1 16 0"/><path d="M12 14l4-5"/><circle cx="12" cy="14" r="1.5" fill="white"/>"#)),
            Icon::Plus => ("plus", svg!(r#"<path d="M12 5v14M5 12h14"/>"#)),
            Icon::Trash => ("trash", svg!(r#"<path d="M4 7h16M9 7V4h6v3M6 7l1 13h10l1-13"/>"#)),
            Icon::Back => ("back", svg!(r#"<path d="M19 12H5M11 18l-6-6 6-6"/>"#)),
            Icon::Refresh => ("refresh", svg!(r#"<path d="M20 12a8 8 0 1 1-2.4-5.7"/><path d="M20 4v5h-5"/>"#)),
            Icon::Check => ("check", svg!(r#"<path d="M5 12l5 5 9-10"/>"#)),
            Icon::Volume => ("volume", svg!(r#"<path d="M4 9h4l5-4v14l-5-4H4z"/><path d="M16.5 8.5a5 5 0 0 1 0 7M19 6a8.5 8.5 0 0 1 0 12"/>"#)),
            Icon::VolumeOff => ("volume-off", svg!(r#"<path d="M4 9h4l5-4v14l-5-4H4z"/><path d="M17 9l5 6M22 9l-5 6"/>"#)),
            Icon::Pause => ("pause", svg!(r#"<rect x="6" y="5" width="4" height="14" rx="1" fill="white"/><rect x="14" y="5" width="4" height="14" rx="1" fill="white"/>"#)),
            Icon::Send => ("send", svg!(r#"<path d="M4 12l16-8-6 16-2.5-6.5z"/>"#)),
        };
        (name, s.as_bytes())
    }

    pub fn image(self, size: f32, tint: Color32) -> egui::Image<'static> {
        let (name, bytes) = self.svg();
        egui::Image::new(ImageSource::Bytes { uri: format!("bytes://corvox/{name}.svg").into(), bytes: bytes.into() })
            .fit_to_exact_size(Vec2::splat(size))
            .tint(tint)
    }
}

/// Frameless icon button tinted with the current text color (brighter on hover).
pub fn button(ui: &mut Ui, icon: Icon, tip: &str) -> Response {
    let col = ui.visuals().widgets.inactive.fg_stroke.color;
    ui.add(egui::Button::image(icon.image(16.0, col)).frame(false)).on_hover_text(tip)
}

pub fn button_tinted(ui: &mut Ui, icon: Icon, tint: Color32, tip: &str) -> Response {
    ui.add(egui::Button::image(icon.image(16.0, tint)).frame(false)).on_hover_text(tip)
}

/// Normal button with an icon on the left.
pub fn labeled(ui: &mut Ui, icon: Icon, text: &str) -> Response {
    let col = ui.visuals().widgets.inactive.fg_stroke.color;
    ui.add(egui::Button::image_and_text(icon.image(14.0, col), text))
}

pub fn small(ui: &mut Ui, icon: Icon, tint: Color32) {
    ui.add(icon.image(14.0, tint));
}
