//! Images pasted into a message: found on the clipboard, then made into an
//! image an agent takes ([`PromptImage`]) — PNG, JPEG, GIF, or WebP, small
//! enough to send.
//!
//! Finding them is cheap and happens as the paste does; preparing one decodes
//! and may re-encode it, so it runs on the background executor.

use gpui::{ClipboardEntry, ClipboardItem, ImageFormat};
use image::{DynamicImage, GenericImageView, ImageReader};
use std::io::Cursor;
use std::path::Path;
use std::sync::Arc;
use tod_agent::PromptImage;

/// Past this many bytes an image is shrunk: base64 grows it by a third, and
/// agents take up to 5 MB.
const MAX_BYTES: usize = 3_750_000;
/// Past this many pixels on the long edge an image is shrunk. Agents scale a
/// larger one down themselves; sending it only costs time.
const MAX_EDGE: u32 = 2_048;

/// An image waiting on the clipboard, as its bytes and what they are.
#[derive(Debug, Clone)]
pub struct ClipboardImage {
    pub format: ImageFormat,
    pub bytes: Vec<u8>,
}

/// An image attached to the message being written: what is sent, and the
/// same bytes to show.
#[derive(Debug, Clone)]
pub struct PendingImage {
    pub prompt: PromptImage,
    pub preview: Arc<gpui::Image>,
}

impl PendingImage {
    /// An image that was already prepared, attached again.
    pub fn from_prompt(prompt: PromptImage) -> Self {
        let format = ImageFormat::from_mime_type(&prompt.mime_type).unwrap_or(ImageFormat::Png);
        Self {
            preview: Arc::new(gpui::Image::from_bytes(format, prompt.data.clone())),
            prompt,
        }
    }
}

/// The images a paste would attach. None when the clipboard holds text, so
/// text copied with a picture of itself (as office apps do) pastes as text.
/// Image files copied in a file manager count, read from disk.
pub fn clipboard_images(item: &ClipboardItem) -> Vec<ClipboardImage> {
    let entries = item.entries();
    if entries
        .iter()
        .any(|entry| matches!(entry, ClipboardEntry::String(s) if !s.text().trim().is_empty()))
    {
        return Vec::new();
    }
    let mut images = Vec::new();
    for entry in entries {
        match entry {
            ClipboardEntry::Image(image) => images.push(ClipboardImage {
                format: image.format,
                bytes: image.bytes.clone(),
            }),
            ClipboardEntry::ExternalPaths(paths) => {
                images.extend(paths.paths().iter().filter_map(|path| image_file(path)));
            }
            ClipboardEntry::String(_) => {}
        }
    }
    images
}

/// An image file's contents, when its extension names an image format.
fn image_file(path: &Path) -> Option<ClipboardImage> {
    let format = match path.extension()?.to_str()?.to_ascii_lowercase().as_str() {
        "png" => ImageFormat::Png,
        "jpg" | "jpeg" => ImageFormat::Jpeg,
        "gif" => ImageFormat::Gif,
        "webp" => ImageFormat::Webp,
        "bmp" => ImageFormat::Bmp,
        "tif" | "tiff" => ImageFormat::Tiff,
        _ => return None,
    };
    let bytes = std::fs::read(path).ok()?;
    Some(ClipboardImage { format, bytes })
}

/// Make `image` into one an agent takes. Runs off the UI thread.
pub fn prepare(image: ClipboardImage) -> Result<PendingImage, String> {
    let (mime_type, data) = encode(image)?;
    Ok(PendingImage::from_prompt(PromptImage {
        mime_type: mime_type.to_string(),
        data,
    }))
}

fn encode(image: ClipboardImage) -> Result<(&'static str, Vec<u8>), String> {
    let sendable = match image.format {
        ImageFormat::Png => Some("image/png"),
        ImageFormat::Jpeg => Some("image/jpeg"),
        ImageFormat::Gif => Some("image/gif"),
        ImageFormat::Webp => Some("image/webp"),
        _ => None,
    };
    let reader = || {
        ImageReader::new(Cursor::new(&image.bytes))
            .with_guessed_format()
            .map_err(|e| format!("reading the pasted image: {e}"))
    };
    if let Some(mime_type) = sendable
        && image.bytes.len() <= MAX_BYTES
        && reader()?
            .into_dimensions()
            .is_ok_and(|(w, h)| w.max(h) <= MAX_EDGE)
    {
        return Ok((mime_type, image.bytes));
    }
    let decoded = reader()?
        .decode()
        .map_err(|e| format!("the pasted image could not be read: {e}"))?;
    let (width, height) = decoded.dimensions();
    let decoded = if width.max(height) > MAX_EDGE {
        decoded.resize(MAX_EDGE, MAX_EDGE, image::imageops::FilterType::Triangle)
    } else {
        decoded
    };
    let png = write(&decoded, image::ImageFormat::Png)?;
    if png.len() <= MAX_BYTES {
        return Ok(("image/png", png));
    }
    // A photo-like image is smaller as a JPEG, which has no alpha.
    let jpeg = write(&DynamicImage::ImageRgb8(decoded.to_rgb8()), image::ImageFormat::Jpeg)?;
    if jpeg.len() <= MAX_BYTES {
        return Ok(("image/jpeg", jpeg));
    }
    Err("the pasted image is too large to send".to_string())
}

fn write(image: &DynamicImage, format: image::ImageFormat) -> Result<Vec<u8>, String> {
    let mut out = Vec::new();
    image
        .write_to(&mut Cursor::new(&mut out), format)
        .map_err(|e| format!("encoding the pasted image: {e}"))?;
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::{Rgba, RgbaImage};

    fn bytes(image: &RgbaImage, format: image::ImageFormat) -> Vec<u8> {
        write(&DynamicImage::ImageRgba8(image.clone()), format).unwrap()
    }

    #[test]
    fn a_small_png_is_sent_as_it_is() {
        let png = bytes(&RgbaImage::from_pixel(4, 3, Rgba([1, 2, 3, 255])), image::ImageFormat::Png);
        let pending = prepare(ClipboardImage {
            format: ImageFormat::Png,
            bytes: png.clone(),
        })
        .unwrap();
        assert_eq!(pending.prompt.mime_type, "image/png");
        assert_eq!(pending.prompt.data, png);
    }

    #[test]
    fn a_bitmap_is_sent_as_a_png() {
        // What a screenshot on Windows is on the clipboard.
        let bmp = bytes(&RgbaImage::from_pixel(5, 5, Rgba([9, 9, 9, 255])), image::ImageFormat::Bmp);
        let pending = prepare(ClipboardImage {
            format: ImageFormat::Bmp,
            bytes: bmp,
        })
        .unwrap();
        assert_eq!(pending.prompt.mime_type, "image/png");
        let back = image::load_from_memory(&pending.prompt.data).unwrap();
        assert_eq!(back.dimensions(), (5, 5));
    }

    #[test]
    fn a_large_image_is_shrunk() {
        let png = bytes(&RgbaImage::new(MAX_EDGE * 2, 10), image::ImageFormat::Png);
        let pending = prepare(ClipboardImage {
            format: ImageFormat::Png,
            bytes: png,
        })
        .unwrap();
        let back = image::load_from_memory(&pending.prompt.data).unwrap();
        assert_eq!(back.dimensions().0, MAX_EDGE);
    }

    #[test]
    fn text_on_the_clipboard_wins_over_an_image() {
        let image = gpui::Image::from_bytes(ImageFormat::Png, vec![1]);
        let both = ClipboardItem {
            entries: vec![
                ClipboardEntry::String(gpui::ClipboardString::new("hello".into())),
                ClipboardEntry::Image(image.clone()),
            ],
        };
        assert!(clipboard_images(&both).is_empty());
        let alone = ClipboardItem {
            entries: vec![ClipboardEntry::Image(image)],
        };
        assert_eq!(clipboard_images(&alone).len(), 1);
    }

    #[test]
    fn garbage_is_refused() {
        assert!(
            prepare(ClipboardImage {
                format: ImageFormat::Bmp,
                bytes: vec![0; 10],
            })
            .is_err()
        );
    }
}
