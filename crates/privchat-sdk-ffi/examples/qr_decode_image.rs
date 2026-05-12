// R8.6c diagnostic — feed a real image file through the same
// `qr_decode_luma` Rust SDK API the Android / iOS sides call. This
// proves whether rxing can actually decode the QR independent of any
// platform-side bitmap-to-luma conversion bugs.
//
// Usage:
//   cargo run -p privchat-sdk-ffi --example qr_decode_image -- <path.png>
//
// Reads the image, converts to grayscale Y plane (same ITU-R BT.601
// luma formula the Android Bitmap pipeline uses), feeds it to the
// SDK function. Prints the decoded string or the typed error.

use image::imageops::FilterType;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 2 {
        eprintln!("usage: qr_decode_image <path.png|jpg|webp>");
        std::process::exit(2);
    }
    let path = &args[1];

    let img = match image::open(path) {
        Ok(i) => i,
        Err(e) => {
            eprintln!("failed to read image at {path}: {e}");
            std::process::exit(1);
        }
    };

    // Down-scale large photos to keep decoder fast (rxing on full-res
    // phone shots is slow). Mirrors the Android album path's
    // MAX_LONG_EDGE=1280.
    const MAX_LONG_EDGE: u32 = 1280;
    let (w0, h0) = (img.width(), img.height());
    let img = if w0.max(h0) > MAX_LONG_EDGE {
        let scale = MAX_LONG_EDGE as f32 / w0.max(h0) as f32;
        let nw = (w0 as f32 * scale) as u32;
        let nh = (h0 as f32 * scale) as u32;
        eprintln!("input {w0}x{h0} → downscaled to {nw}x{nh}");
        img.resize(nw, nh, FilterType::Triangle)
    } else {
        eprintln!("input {w0}x{h0} (no downscale)");
        img
    };

    let w = img.width();
    let h = img.height();
    let rgb = img.to_rgb8();
    let mut luma = Vec::with_capacity((w * h) as usize);
    for pixel in rgb.pixels() {
        // ITU-R BT.601 luma — same formula used by Android side's
        // bitmapToLuma() and rxing's HybridBinarizer expects.
        let y = (pixel.0[0] as u32 * 299
            + pixel.0[1] as u32 * 587
            + pixel.0[2] as u32 * 114)
            / 1000;
        luma.push(y as u8);
    }
    eprintln!("luma plane: {} bytes (expected {})", luma.len(), (w * h) as usize);

    // Call the same function the FFI / Android / iOS bindings call.
    // Going through the public re-export verifies the symbol path too.
    let result = privchat_sdk_ffi::qr_decode_luma(w, h, luma);
    match result {
        Ok(Some(text)) => {
            println!("OK decoded {} bytes", text.len());
            println!("---");
            println!("{text}");
            println!("---");
        }
        Ok(None) => {
            eprintln!("FAIL: no QR found in image");
            std::process::exit(3);
        }
        Err(e) => {
            eprintln!("ERR: {e:?}");
            std::process::exit(4);
        }
    }
}
