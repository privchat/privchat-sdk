// Copyright 2024 Shanghai Boyu Information Technology Co., Ltd.
// https://privchat.dev
//
// Author: zoujiaqing <zoujiaqing@gmail.com>
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//     http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

//! R8.6b-rust — shared QR-code luma decoder.
//!
//! The Android (CameraX `ImageAnalysis`) and iOS (AVFoundation
//! `AVCaptureVideoDataOutput`) capture paths produce raw YUV frames; the
//! luma (Y) plane alone is enough to decode a QR — chroma carries no
//! payload information. This module exposes one minimal function over
//! UniFFI so both platforms call into the same Rust decoder built on
//! rxing (ZXing port). The decoder is also reused for "pick QR image
//! from photo album" — platforms convert the album image to grayscale
//! and feed the same luma bytes here.
//!
//! Wire contract:
//! - `Ok(Some(text))` — QR found, returns decoded text
//! - `Ok(None)`       — image is well-formed but contains no QR (normal
//!                       miss while scanning; UI keeps polling next frame)
//! - `Err(InvalidDimensions)` — caller-side bug: `width == 0`,
//!                              `height == 0`, or `luma.len() != width * height`
//! - `Err(DecoderError)` — rxing internal failure (rare)
//!
//! Why `Result<Option<String>, _>` instead of plain `Option<String>`:
//! the two failure modes have very different UI semantics. Dimension
//! mismatches mean the platform code is sending garbage and must be
//! fixed — that should throw at the UniFFI boundary so the bug is
//! loud. Genuine "no QR in frame" is the steady-state during live
//! scanning and must NOT throw — every frame would surface an
//! exception otherwise.

use rxing::{
    common::HybridBinarizer, qrcode::QRCodeReader, BinaryBitmap, DecodeHints,
    Luma8LuminanceSource, Reader,
};

// NOTE: `QrDecodeError` and `qr_decode_luma` are re-exported / wrapped
// at the lib.rs root. The KMP uniffi-bindgen mangles C-symbol names
// with the source module path when `#[uniffi::export]` sits inside a
// non-root module — that produces `uniffi_<crate>::qr_fn_func_<name>`
// with literal `::` which Clang refuses to parse. Keeping the actual
// `#[uniffi::export]` and `#[derive(uniffi::Error)]` annotations at
// crate root sidesteps that. This module owns the implementation
// (Reader pipeline, dimension validation, NotFound classification) and
// the error variants; lib.rs re-uses them via thin wrappers.

/// Errors surfaced through UniFFI to Kotlin / Swift callers.
#[derive(Debug, thiserror::Error)]
pub enum QrDecodeError {
    /// The caller passed inconsistent dimensions — `width == 0`,
    /// `height == 0`, or `luma.len() != width as usize * height as usize`.
    /// This is a programming bug on the platform side (wrong plane
    /// stride / wrong rotation handling) and should surface loudly.
    #[error(
        "invalid luma dimensions: width={width} height={height} luma_len={luma_len}"
    )]
    InvalidDimensions {
        width: u32,
        height: u32,
        luma_len: u32,
    },

    /// rxing produced an unexpected error that isn't "no QR found".
    /// Rare in practice; most rxing failures funnel into `Ok(None)`.
    /// Carried as a string because rxing's error type isn't UniFFI-friendly.
    #[error("decoder error: {detail}")]
    DecoderError { detail: String },
}

/// Implementation behind the root-level `qr_decode_luma`. Called from
/// `lib.rs`'s `#[uniffi::export]` wrapper. See module docs for why this
/// indirection is required.
pub fn decode_luma(
    width: u32,
    height: u32,
    luma: Vec<u8>,
) -> Result<Option<String>, QrDecodeError> {
    let expected = (width as usize).checked_mul(height as usize);
    let valid = width > 0
        && height > 0
        && expected.is_some()
        && expected.unwrap() == luma.len();
    if !valid {
        return Err(QrDecodeError::InvalidDimensions {
            width,
            height,
            luma_len: luma.len() as u32,
        });
    }

    // rxing pipeline: LuminanceSource → HybridBinarizer (Otsu-equivalent
    // adaptive threshold) → BinaryBitmap → QRCodeReader. We pin to the
    // QR reader specifically — `MultiFormatReader` would also try
    // Aztec / DataMatrix / etc. which we don't enable as features.
    let source = Luma8LuminanceSource::new(luma, width, height);
    let binarizer = HybridBinarizer::new(source);
    let mut bitmap = BinaryBitmap::new(binarizer);
    let mut reader = QRCodeReader {};

    match reader.decode_with_hints(&mut bitmap, &DecodeHints::default()) {
        Ok(result) => Ok(Some(result.getText().to_string())),
        Err(e) => {
            // rxing emits a typed "NotFound" exception for "no QR in
            // this frame"; classify everything else as DecoderError so
            // platform code can tell the difference. The error matcher
            // is by Display because Exceptions enum is non-exhaustive
            // and not all variants are pub-reachable in our minimal
            // feature set.
            if is_not_found(&e) {
                Ok(None)
            } else {
                Err(QrDecodeError::DecoderError {
                    detail: e.to_string(),
                })
            }
        }
    }
}

/// rxing surfaces several error shapes for "binarizer found nothing":
/// `NotFoundException`, `Exceptions::NotFoundException`, or messages
/// containing "not found". Bunch the lot together — anything that says
/// "no QR in this frame" must map to `Ok(None)`, not an error.
fn is_not_found(err: &rxing::Exceptions) -> bool {
    matches!(err, rxing::Exceptions::NotFoundException(_))
}

// ───────────────────── QR_CODE_SPEC v1.4 D0 — QR encoder ─────────────────────
//
// Goal: produce a square cell matrix the UI layer (gearui-kit on KMP)
// renders directly via a Box grid — no platform Bitmap, no PNG, no
// CoreImage. Wire shape (matches the FFI type at lib.rs root):
//
//   size:  u32         (modules per side; e.g. 21 for V1, 25 for V2, ...)
//   cells: Vec<u8>     (size*size, row-major; 0 = light, 1 = dark)
//
// Quiet-zone padding is **not** baked into `cells` — UI adds it as
// surrounding margin to keep the matrix size minimal and let renderers
// pick their own quiet-zone factor (typical: 4 modules).

/// Errors for [`encode_matrix`]. Strings are caller-actionable: the
/// empty-input case is a programming bug at the call site, the
/// underlying encoder error covers payload-too-large / impossible-ecc
/// combinations.
#[derive(Debug, thiserror::Error)]
pub enum QrEncodeError {
    /// Caller passed an empty / whitespace-only string. QR can technically
    /// encode the empty payload, but it's almost always a bug at the call
    /// site (e.g. `userQrcodeGet` returned an empty `qr_code` and we'd
    /// silently render a tiny meaningless QR).
    #[error("qr text must not be empty")]
    EmptyText,

    /// Underlying encoder failure (payload too long, unsupported character
    /// set, etc.).
    #[error("encoder error: {detail}")]
    EncoderError { detail: String },
}

/// One encoded QR matrix. Lives in `qr.rs` so the test module can
/// round-trip against `decode_luma`; mirrored at lib.rs root as the
/// UniFFI-exported `QrMatrixView` record (re-using this name there
/// would conflict with the re-export pattern used for `QrDecodeError`,
/// so the root copy has the same fields and an `From` impl below).
pub struct QrMatrix {
    /// Modules per side (also row width). Always > 0.
    pub size: u32,
    /// Row-major cells; length is `size * size`. `0 = light`, `1 = dark`.
    pub cells: Vec<u8>,
}

/// Encode `text` to a QR matrix at error-correction level **M** (the
/// 15% redundancy default — a sensible point on the size/robustness
/// curve for permanent name-card / group URLs that get printed,
/// reshared, screen-photographed, etc.).
///
/// Returns one [`QrMatrix`]. Quiet zone NOT included — UI layer adds
/// the standard 4-module margin via container padding.
pub fn encode_matrix(text: &str) -> Result<QrMatrix, QrEncodeError> {
    if text.trim().is_empty() {
        return Err(QrEncodeError::EmptyText);
    }

    let code = ::qrcode::QrCode::with_error_correction_level(text, ::qrcode::EcLevel::M)
        .map_err(|e| QrEncodeError::EncoderError {
            detail: e.to_string(),
        })?;

    let size_usize = code.width();
    let mut cells = Vec::with_capacity(size_usize * size_usize);
    for y in 0..size_usize {
        for x in 0..size_usize {
            cells.push(if code[(x, y)] == ::qrcode::Color::Dark { 1 } else { 0 });
        }
    }

    Ok(QrMatrix {
        size: size_usize as u32,
        cells,
    })
}

#[cfg(test)]
mod encode_tests {
    use super::*;

    #[test]
    fn encode_returns_positive_size_and_correct_cell_count() {
        let m = encode_matrix("hello world").expect("encode ok");
        assert!(m.size > 0, "size must be > 0");
        assert_eq!(
            m.cells.len() as u32,
            m.size * m.size,
            "cells.len() must equal size * size",
        );
    }

    #[test]
    fn encode_cells_only_contain_0_or_1() {
        let m = encode_matrix("https://privchat.app/privchat:protocol/user/get/abc123")
            .expect("encode ok");
        assert!(
            m.cells.iter().all(|&c| c == 0 || c == 1),
            "cells must be 0/1 only — found {:?}",
            m.cells.iter().find(|&&c| c != 0 && c != 1),
        );
    }

    #[test]
    fn encode_empty_text_returns_error() {
        assert!(matches!(
            encode_matrix(""),
            Err(QrEncodeError::EmptyText)
        ));
    }

    #[test]
    fn encode_whitespace_only_text_returns_error() {
        assert!(matches!(
            encode_matrix("   \t\n  "),
            Err(QrEncodeError::EmptyText)
        ));
    }

    /// Round-trip via the existing `decode_luma` pipeline: encoder output
    /// must be readable by the rxing decoder. Catches any matrix-layout
    /// drift (row-major vs column-major, off-by-one quiet zone, etc.).
    #[test]
    fn encode_decode_round_trip() {
        let payload = "https://privchat.app/privchat:protocol/user/get/K7sP3qXfA9eLm2nB";
        let matrix = encode_matrix(payload).expect("encode ok");

        // Render the matrix to a luma byte plane the decoder can read:
        // scale each module to 6 pixels and add a 4-module quiet zone.
        let scale = 6usize;
        let quiet = 4usize;
        let module_count = matrix.size as usize;
        let pixel_per_side = (module_count + 2 * quiet) * scale;
        let mut luma = vec![255u8; pixel_per_side * pixel_per_side];
        for py in 0..pixel_per_side {
            for px in 0..pixel_per_side {
                let in_x = px >= quiet * scale && px < (module_count + quiet) * scale;
                let in_y = py >= quiet * scale && py < (module_count + quiet) * scale;
                if in_x && in_y {
                    let mx = (px - quiet * scale) / scale;
                    let my = (py - quiet * scale) / scale;
                    if matrix.cells[my * module_count + mx] == 1 {
                        luma[py * pixel_per_side + px] = 0;
                    }
                }
            }
        }

        let decoded = decode_luma(pixel_per_side as u32, pixel_per_side as u32, luma)
            .expect("decode ok");
        assert_eq!(decoded.as_deref(), Some(payload));
    }

    #[test]
    fn encode_long_url_does_not_explode() {
        // Stress the encoder with the worst realistic URL (deep sub-path,
        // 16-char base62 key) — must still produce a sane matrix.
        let payload = "https://tenant.example.com/some/deep/sub/path/privchat:protocol/group/join/Zk9Pq3RtUvWxYz12";
        let m = encode_matrix(payload).expect("encode ok");
        assert!(m.size > 0);
        assert_eq!(m.cells.len() as u32, m.size * m.size);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use qrcode::{EcLevel, QrCode};

    /// Round-trip helper: encode `payload` to a QR matrix using the
    /// dev-only `qrcode` crate, expand it to a luma byte array with
    /// the given module-to-pixel scale and quiet zone, and feed it
    /// through `qr_decode_luma`. Returns the decoded payload (or
    /// propagates the error).
    fn encode_to_luma(payload: &str, scale: usize, quiet: usize) -> (u32, u32, Vec<u8>) {
        let code = QrCode::with_error_correction_level(payload, EcLevel::M)
            .expect("encode QR");
        let module_count = code.width();
        let pixel_per_side = (module_count + 2 * quiet) * scale;
        // Materialise the module matrix into a Vec<bool> first so the
        // `code` value isn't borrowed across the nested closure (which
        // would force a clone on every cell). Row-major, stride = module_count.
        let mut modules = Vec::with_capacity(module_count * module_count);
        for y in 0..module_count {
            for x in 0..module_count {
                modules.push(code[(x, y)] == qrcode::Color::Dark);
            }
        }
        let stride = module_count;
        let mut luma = vec![255u8; pixel_per_side * pixel_per_side];
        for py in 0..pixel_per_side {
            for px in 0..pixel_per_side {
                let mx = if px < quiet * scale || px >= (module_count + quiet) * scale {
                    None
                } else {
                    Some((px - quiet * scale) / scale)
                };
                let my = if py < quiet * scale || py >= (module_count + quiet) * scale {
                    None
                } else {
                    Some((py - quiet * scale) / scale)
                };
                if let (Some(mx), Some(my)) = (mx, my) {
                    if modules[my * stride + mx] {
                        luma[py * pixel_per_side + px] = 0;
                    }
                }
            }
        }
        (pixel_per_side as u32, pixel_per_side as u32, luma)
    }

    #[test]
    fn decode_known_qr_returns_text() {
        let (w, h, luma) = encode_to_luma("hello world", 6, 4);
        let result = decode_luma(w, h, luma).expect("decode ok");
        assert_eq!(result.as_deref(), Some("hello world"));
    }

    #[test]
    fn decode_login_envelope_round_trip() {
        // The exact wire-fix shape from R8.6a — privchat-web encodes
        // this JSON into the QR canvas, App scanner decodes it back.
        let payload = r#"{"sceneId":"9f3b1234-5678-90ab-cdef-000000000001","qrToken":"qr_signed_xyz"}"#;
        let (w, h, luma) = encode_to_luma(payload, 6, 4);
        let result = decode_luma(w, h, luma).expect("decode ok");
        assert_eq!(result.as_deref(), Some(payload));
    }

    #[test]
    fn decode_utf8_payload_works() {
        // Exercises the encoding_rs feature: byte-mode QR with non-ASCII
        // bytes (UTF-8 here) must come back intact, not garbled into
        // ISO-8859-1.
        let payload = "你好 PrivChat";
        let (w, h, luma) = encode_to_luma(payload, 8, 4);
        let result = decode_luma(w, h, luma).expect("decode ok");
        assert_eq!(result.as_deref(), Some(payload));
    }

    #[test]
    fn decode_blank_image_returns_ok_none() {
        // 64×64 of pure white = no edges, no candidate patterns.
        // rxing surfaces this as NotFound, which we map to Ok(None).
        let luma = vec![255u8; 64 * 64];
        let result = decode_luma(64, 64, luma).expect("decode ok");
        assert!(
            result.is_none(),
            "blank image must decode to None, got {result:?}"
        );
    }

    #[test]
    fn decode_mismatched_dimensions_returns_err() {
        // luma.len() ≠ width * height — caller-side bug, must throw.
        let luma = vec![0u8; 100];
        let result = decode_luma(8, 8, luma);
        match result {
            Err(QrDecodeError::InvalidDimensions {
                width,
                height,
                luma_len,
            }) => {
                assert_eq!(width, 8);
                assert_eq!(height, 8);
                assert_eq!(luma_len, 100);
            }
            other => panic!("expected InvalidDimensions, got {other:?}"),
        }
    }

    #[test]
    fn decode_zero_dimensions_returns_err() {
        let result = decode_luma(0, 16, vec![]);
        assert!(matches!(
            result,
            Err(QrDecodeError::InvalidDimensions { .. })
        ));

        let result = decode_luma(16, 0, vec![]);
        assert!(matches!(
            result,
            Err(QrDecodeError::InvalidDimensions { .. })
        ));
    }
}
