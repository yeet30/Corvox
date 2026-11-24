//! Windows: gives corvox.exe its icon (Explorer, shortcuts, pinned taskbar buttons) and
//! version details. The icon is drawn by the same code the app uses at runtime.

include!("src/app_icon.rs");

fn main() {
    println!("cargo:rerun-if-changed=src/app_icon.rs");
    println!("cargo:rerun-if-changed=build.rs");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }
    let out = std::path::PathBuf::from(std::env::var("OUT_DIR").unwrap());
    let ico = out.join("corvox.ico");
    std::fs::write(&ico, ico_file(&[16, 20, 24, 32, 40, 48, 64, 256])).unwrap();
    let mut res = winresource::WindowsResource::new();
    res.set_icon(ico.to_str().unwrap()).set("FileDescription", "Corvox").set("ProductName", "Corvox");
    // Without the Windows SDK's resource compiler the app still builds, just without an icon.
    if let Err(e) = res.compile() {
        println!("cargo:warning=corvox.exe gets no icon: {e}");
    }
}

/// An .ico file with one 32-bit image per size (BMP format, which every Windows reads).
fn ico_file(sizes: &[u32]) -> Vec<u8> {
    let images: Vec<Vec<u8>> = sizes.iter().map(|&s| dib(s)).collect();
    let mut f = Vec::new();
    f.extend(0u16.to_le_bytes()); // reserved
    f.extend(1u16.to_le_bytes()); // type: icon
    f.extend((sizes.len() as u16).to_le_bytes());
    let mut offset = 6 + 16 * sizes.len() as u32;
    for (&s, img) in sizes.iter().zip(&images) {
        let dim = if s >= 256 { 0 } else { s as u8 }; // 0 means 256
        f.extend([dim, dim, 0, 0]);
        f.extend(1u16.to_le_bytes()); // planes
        f.extend(32u16.to_le_bytes()); // bits per pixel
        f.extend((img.len() as u32).to_le_bytes());
        f.extend(offset.to_le_bytes());
        offset += img.len() as u32;
    }
    for img in images {
        f.extend(img);
    }
    f
}

/// One icon image: a BITMAPINFOHEADER, BGRA pixels bottom-up, then an all-clear AND mask
/// (transparency comes from the alpha channel).
fn dib(size: u32) -> Vec<u8> {
    let (rgba, n) = app_icon_rgba(size);
    let mask_row = n.div_ceil(32) * 4;
    let mut d = Vec::new();
    d.extend(40u32.to_le_bytes()); // header size
    d.extend((n as i32).to_le_bytes());
    d.extend((2 * n as i32).to_le_bytes()); // color + mask height
    d.extend(1u16.to_le_bytes());
    d.extend(32u16.to_le_bytes());
    d.extend(0u32.to_le_bytes()); // no compression
    d.extend((n * n * 4 + mask_row * n).to_le_bytes());
    d.extend([0u8; 16]); // resolution, palette
    for y in (0..n).rev() {
        for x in 0..n {
            let i = ((y * n + x) * 4) as usize;
            d.extend([rgba[i + 2], rgba[i + 1], rgba[i], rgba[i + 3]]);
        }
    }
    d.extend(std::iter::repeat_n(0u8, (mask_row * n) as usize));
    d
}
