// Corvox's app icon, drawn in code. Shared by the app (`icons.rs`) and `build.rs`, which
// embeds it in the Windows exe, so this file must not use any crate.

/// Corvox's app icon (window, taskbar and tray): a white microphone on a blue rounded
/// square, drawn here so no image files are needed. Returns RGBA pixels, `size`×`size`.
pub fn app_icon_rgba(size: u32) -> (Vec<u8>, u32) {
    // Shapes in 0..1 coordinates; each answers "is this point inside?".
    fn rounded_rect(x: f32, y: f32, l: f32, t: f32, r: f32, b: f32, rad: f32) -> bool {
        let cx = x.clamp(l + rad, r - rad);
        let cy = y.clamp(t + rad, b - rad);
        x >= l && x <= r && y >= t && y <= b && (x - cx).powi(2) + (y - cy).powi(2) <= rad * rad
    }
    fn mic(x: f32, y: f32) -> bool {
        let capsule = rounded_rect(x, y, 0.385, 0.17, 0.615, 0.58, 0.115);
        let (dx, dy) = (x - 0.5, y - 0.44);
        let d = (dx * dx + dy * dy).sqrt();
        let cradle = dy >= 0.0 && (0.205..=0.265).contains(&d);
        let stem = (0.47..=0.53).contains(&x) && (0.69..=0.80).contains(&y);
        let base = rounded_rect(x, y, 0.35, 0.775, 0.65, 0.835, 0.03);
        capsule || cradle || stem || base
    }
    let bg = [0x2F_u8, 0x6F, 0xDE];
    let ss = 4; // 4×4 samples per pixel for smooth edges
    let mut out = Vec::with_capacity((size * size * 4) as usize);
    for py in 0..size {
        for px in 0..size {
            let (mut cover, mut white) = (0u32, 0u32);
            for sy in 0..ss {
                for sx in 0..ss {
                    let x = (px as f32 + (sx as f32 + 0.5) / ss as f32) / size as f32;
                    let y = (py as f32 + (sy as f32 + 0.5) / ss as f32) / size as f32;
                    if rounded_rect(x, y, 0.0, 0.0, 1.0, 1.0, 0.22) {
                        cover += 1;
                        if mic(x, y) {
                            white += 1;
                        }
                    }
                }
            }
            let n = (ss * ss) as f32;
            let w = if cover > 0 { white as f32 / cover as f32 } else { 0.0 };
            for c in bg {
                out.push((c as f32 + (255.0 - c as f32) * w).round() as u8);
            }
            out.push((255.0 * cover as f32 / n).round() as u8);
        }
    }
    (out, size)
}
