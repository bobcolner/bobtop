//! Image preview — decode + downsample.
//!
//! The preview pipeline produces a decoded `DynamicImage`; the actual
//! half-block rasterization happens at draw time when the caller knows
//! the destination cell rect (see `crate::ui::rasterize_image`). That
//! split lets us cache a single decoded image and render it at any
//! pane size without re-decoding.

use std::path::Path;
use std::sync::Arc;

use ::image::{imageops::FilterType, DynamicImage, GenericImageView};

use super::{Preview, PreviewBody, PreviewKind};

/// Largest dimension we keep in the cache. 1024 px keeps memory under
/// ~4 MiB per image (1024×1024×4) and preserves enough detail for
/// half-block rendering on every reasonable pane size — a 4K image
/// downsampled to 1024 still oversamples a 200-cell-wide pane.
const MAX_DIM: u32 = 1024;

pub fn decode_image(path: &Path) -> Result<Preview, String> {
    let img = ::image::open(path).map_err(|e| format!("decode: {e}"))?;
    let (w, h) = img.dimensions();
    let downsampled = if w.max(h) > MAX_DIM {
        let scale = MAX_DIM as f32 / w.max(h) as f32;
        let nw = ((w as f32 * scale).round() as u32).max(1);
        let nh = ((h as f32 * scale).round() as u32).max(1);
        img.resize_exact(nw, nh, FilterType::Triangle)
    } else {
        img
    };
    // RGB8 conversion happens here so draw-time rasterization can
    // index pixels without reformat overhead. Alpha is composited
    // against black — adequate for preview, perfect compositing onto
    // theme bg can come later.
    let rgba = DynamicImage::ImageRgb8(downsampled.to_rgb8());
    Ok(Preview {
        kind: PreviewKind::Image,
        body: PreviewBody::Image(Arc::new(rgba)),
        source_lines: 0,
        note: Some(format!("{}×{}", w, h)),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_tiny_png(name: &str) -> std::path::PathBuf {
        // Encode a 2×2 RGB image through the same image crate we
        // depend on at runtime. Avoids hand-rolled PNG bytes that
        // varied across zlib implementations.
        let mut img = ::image::RgbImage::new(2, 2);
        img.put_pixel(0, 0, ::image::Rgb([255, 0, 0]));
        img.put_pixel(1, 0, ::image::Rgb([0, 255, 0]));
        img.put_pixel(0, 1, ::image::Rgb([0, 0, 255]));
        img.put_pixel(1, 1, ::image::Rgb([255, 255, 0]));
        let mut p = std::env::temp_dir();
        p.push(format!("gfb-img-{}-{}.png", std::process::id(), name));
        img.save(&p).unwrap();
        p
    }

    #[test]
    fn decodes_png_to_image_body() {
        let path = write_tiny_png("tiny");
        let p = decode_image(&path).unwrap();
        assert_eq!(p.kind, PreviewKind::Image);
        match &p.body {
            PreviewBody::Image(img) => {
                assert!(img.width() > 0);
                assert!(img.height() > 0);
            }
            _ => panic!("expected Image body"),
        }
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn missing_file_returns_error() {
        let result = decode_image(Path::new("/nonexistent/file.png"));
        assert!(result.is_err());
    }
}
