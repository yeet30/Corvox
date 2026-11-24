// Corvox's app icon, drawn in code. Shared by the app (`icons.rs`) and `build.rs`, which
// embeds it in the Windows exe, so this file must not use any crate.

/// Corvox's app icon (window, taskbar, tray and exe): a black microphone whose head is a crow
/// calling with its beak open, on a white rounded square. Returns RGBA pixels, `size`×`size`.
///
/// Shapes are in design units, 0–100 across.
pub fn app_icon_rgba(size: u32) -> (Vec<u8>, u32) {
    let head = crow_head();
    let ink = [0x11_u8, 0x11, 0x11];
    let ss = if size >= 128 { 2 } else { 4 }; // samples per pixel side, for smooth edges
    let mut out = Vec::with_capacity((size * size * 4) as usize);
    for py in 0..size {
        for px in 0..size {
            let (mut cover, mut dark) = (0u32, 0u32);
            for sy in 0..ss {
                for sx in 0..ss {
                    let x = (px as f32 + (sx as f32 + 0.5) / ss as f32) * 100.0 / size as f32;
                    let y = (py as f32 + (sy as f32 + 0.5) / ss as f32) * 100.0 / size as f32;
                    if icon_tile(x, y) {
                        cover += 1;
                        if icon_ink(x, y, &head) {
                            dark += 1;
                        }
                    }
                }
            }
            let d = if cover > 0 { dark as f32 / cover as f32 } else { 0.0 };
            for c in ink {
                out.push((255.0 - (255.0 - c as f32) * d).round() as u8);
            }
            out.push((255.0 * cover as f32 / (ss * ss) as f32).round() as u8);
        }
    }
    (out, size)
}

/// The white rounded square behind everything.
fn icon_tile(x: f32, y: f32) -> bool {
    let r = 22.0;
    let cx = x.clamp(r, 100.0 - r);
    let cy = y.clamp(r, 100.0 - r);
    (0.0..=100.0).contains(&x) && (0.0..=100.0).contains(&y) && (x - cx).powi(2) + (y - cy).powi(2) <= r * r
}

/// Whether a point is black: the crow head (minus its eye) or the mic stand.
fn icon_ink(x: f32, y: f32, head: &[(f32, f32)]) -> bool {
    if (x - 54.0).powi(2) + (y - 23.5).powi(2) <= 2.3 * 2.3 {
        return false; // the eye
    }
    if (37.0..=82.0).contains(&x) && (14.0..=59.0).contains(&y) && inside_polygon(head, x, y) {
        return true;
    }
    // The stand: a U-shaped cradle, a stem and a base, 5 units thick with round ends.
    let half = 2.5;
    let (dx, dy) = (x - 50.0, y - 50.0);
    let cradle = if dy >= 0.0 {
        ((dx * dx + dy * dy).sqrt() - 20.0).abs() <= half
    } else {
        segment_distance(x, y, (30.0, 50.0), (30.0, 50.0)) <= half || segment_distance(x, y, (70.0, 50.0), (70.0, 50.0)) <= half
    };
    cradle || segment_distance(x, y, (50.0, 70.0), (50.0, 81.0)) <= half || segment_distance(x, y, (38.0, 83.0), (62.0, 83.0)) <= half
}

/// Outline of the crow head: a microphone capsule with a crow's crown and an open beak
/// pointing right. Curves are flattened into short straight lines.
fn crow_head() -> Vec<(f32, f32)> {
    fn cubic(p: &mut Vec<(f32, f32)>, a: (f32, f32), b: (f32, f32), c: (f32, f32), d: (f32, f32)) {
        for i in 1..=10 {
            let t = i as f32 / 10.0;
            let u = 1.0 - t;
            let w = [u * u * u, 3.0 * u * u * t, 3.0 * u * t * t, t * t * t];
            p.push((w[0] * a.0 + w[1] * b.0 + w[2] * c.0 + w[3] * d.0, w[0] * a.1 + w[1] * b.1 + w[2] * c.1 + w[3] * d.1));
        }
    }
    fn quad(p: &mut Vec<(f32, f32)>, a: (f32, f32), b: (f32, f32), c: (f32, f32)) {
        for i in 1..=4 {
            let t = i as f32 / 4.0;
            let u = 1.0 - t;
            p.push((u * u * a.0 + 2.0 * u * t * b.0 + t * t * c.0, u * u * a.1 + 2.0 * u * t * b.1 + t * t * c.1));
        }
    }
    let mut p = vec![(38.0, 46.0), (38.0, 30.0)];
    cubic(&mut p, (38.0, 30.0), (38.0, 20.0), (45.0, 14.0), (53.0, 15.0)); // back of the head
    cubic(&mut p, (53.0, 15.0), (57.0, 15.0), (60.0, 16.5), (61.5, 18.5)); // forehead, sloping into the beak
    cubic(&mut p, (61.5, 18.5), (68.0, 17.8), (76.0, 19.5), (81.5, 23.8)); // heavy upper beak, hooked tip
    p.push((78.5, 24.2));
    p.push((64.5, 27.8)); // corner of the open mouth
    p.push((77.5, 31.0)); // lower beak
    quad(&mut p, (77.5, 31.0), (78.8, 31.8), (77.2, 32.8)); // its tip
    cubic(&mut p, (77.2, 32.8), (72.0, 34.2), (66.0, 35.2), (63.2, 36.2));
    cubic(&mut p, (63.2, 36.2), (62.4, 39.0), (62.0, 41.5), (62.0, 44.0)); // throat
    p.push((62.0, 46.0));
    for i in 1..16 {
        let a = std::f32::consts::PI * i as f32 / 16.0; // round bottom, like a capsule
        p.push((50.0 + 12.0 * a.cos(), 46.0 + 12.0 * a.sin()));
    }
    p
}

fn inside_polygon(poly: &[(f32, f32)], x: f32, y: f32) -> bool {
    let mut inside = false;
    let mut j = poly.len() - 1;
    for i in 0..poly.len() {
        let ((xi, yi), (xj, yj)) = (poly[i], poly[j]);
        if (yi > y) != (yj > y) && x < (xj - xi) * (y - yi) / (yj - yi) + xi {
            inside = !inside;
        }
        j = i;
    }
    inside
}

fn segment_distance(x: f32, y: f32, a: (f32, f32), b: (f32, f32)) -> f32 {
    let (vx, vy) = (b.0 - a.0, b.1 - a.1);
    let len2 = vx * vx + vy * vy;
    let t = if len2 > 0.0 { (((x - a.0) * vx + (y - a.1) * vy) / len2).clamp(0.0, 1.0) } else { 0.0 };
    let (cx, cy) = (a.0 + t * vx, a.1 + t * vy);
    ((x - cx).powi(2) + (y - cy).powi(2)).sqrt()
}
