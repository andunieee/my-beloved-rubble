//! Previews of image files: small thumbnails for the file list, cached in
//! `.mbr/thumbs/<hash>.png` (by content, like the blobs: they outlive
//! renames, and the local copy being dropped), and larger previews made
//! on demand. Whether a blob is an image is told by its first bytes, not
//! its name.

use crate::repo;
use std::io::Read;
use std::path::{Path, PathBuf};

/// Where thumbnails are cached, relative to the folder's root.
pub const THUMBS_DIR: &str = ".mbr/thumbs";
/// The longest side of a thumbnail, in pixels.
pub const THUMB_SIZE: u32 = 96;
/// Images larger than this aren't decoded for a preview.
const MAX_IMAGE_BYTES: u64 = 64 << 20;

/// An image ready to show: RGBA pixels, row by row.
#[derive(Debug, Clone)]
pub struct Preview {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

/// What became of a request for a preview.
#[derive(Debug, Clone)]
pub enum Outcome {
    Ready(Preview),
    /// The blob isn't an image mbr can show (or is too large).
    NotAnImage,
    /// The blob isn't here, and no thumbnail of it was made before.
    Missing,
}

fn cache_path(root: &Path, hash: &str) -> PathBuf {
    root.join(THUMBS_DIR).join(format!("{hash}.png"))
}

/// The thumbnail of blob `hash`: from the cache, or made from the local
/// blob (and cached). Slow: run it off the UI thread.
pub fn thumbnail(root: &Path, hash: &str) -> Outcome {
    let cached = cache_path(root, hash);
    if let Ok(image) = image::open(&cached) {
        return Outcome::Ready(to_preview(image));
    }
    let blob = repo::blob_path_in(root, hash);
    if let Err(outcome) = check(&blob) {
        return outcome;
    }
    let Ok(image) = open_image(&blob) else {
        return Outcome::NotAnImage;
    };
    let thumb = image.thumbnail(THUMB_SIZE, THUMB_SIZE);
    if let Some(dir) = cached.parent()
        && std::fs::create_dir_all(dir).is_ok()
    {
        // Written aside and moved in place, so a half-written one is
        // never read back.
        let tmp = cached.with_extension("png.tmp");
        if thumb.save_with_format(&tmp, image::ImageFormat::Png).is_ok() {
            std::fs::rename(&tmp, &cached).ok();
        }
    }
    Outcome::Ready(to_preview(thumb))
}

/// The image in blob `hash`, its longest side at most `max` pixels (not
/// cached). Slow: run it off the UI thread.
pub fn preview(root: &Path, hash: &str, max: u32) -> Outcome {
    let blob = repo::blob_path_in(root, hash);
    if let Err(outcome) = check(&blob) {
        return outcome;
    }
    match open_image(&blob) {
        Ok(image) if image.width() > max || image.height() > max => {
            Outcome::Ready(to_preview(image.thumbnail(max, max)))
        }
        Ok(image) => Outcome::Ready(to_preview(image)),
        Err(_) => Outcome::NotAnImage,
    }
}

/// Whether the file at `path` is an image worth decoding, from its size
/// and first bytes; if not, why.
fn check(path: &Path) -> Result<(), Outcome> {
    let meta = std::fs::metadata(path).map_err(|_| Outcome::Missing)?;
    if meta.len() > MAX_IMAGE_BYTES {
        return Err(Outcome::NotAnImage);
    }
    let mut head = [0u8; 32];
    let n = std::fs::File::open(path)
        .and_then(|mut f| f.read(&mut head))
        .map_err(|_| Outcome::Missing)?;
    match image::guess_format(&head[..n]) {
        Ok(_) => Ok(()),
        Err(_) => Err(Outcome::NotAnImage),
    }
}

fn open_image(path: &Path) -> Result<image::DynamicImage, String> {
    image::ImageReader::open(path)
        .and_then(|r| r.with_guessed_format())
        .map_err(|e| e.to_string())?
        .decode()
        .map_err(|e| e.to_string())
}

fn to_preview(image: image::DynamicImage) -> Preview {
    let rgba = image.into_rgba8();
    Preview {
        width: rgba.width(),
        height: rgba.height(),
        rgba: rgba.into_raw(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn thumbnails_are_made_cached_and_outlive_the_blob() {
        let root = std::env::temp_dir().join(format!("mbr-thumbs-{}", std::process::id()));
        std::fs::remove_dir_all(&root).ok();
        let hash = "ab".repeat(32);
        let blob = repo::blob_path_in(&root, &hash);
        std::fs::create_dir_all(blob.parent().unwrap()).unwrap();
        image::RgbaImage::from_pixel(400, 200, image::Rgba([255, 0, 0, 255]))
            .save_with_format(&blob, image::ImageFormat::Png)
            .unwrap();

        let Outcome::Ready(thumb) = thumbnail(&root, &hash) else {
            panic!("no thumbnail");
        };
        assert_eq!((thumb.width, thumb.height), (THUMB_SIZE, THUMB_SIZE / 2));
        assert_eq!(&thumb.rgba[..4], &[255, 0, 0, 255]);
        let Outcome::Ready(big) = preview(&root, &hash, 100) else {
            panic!("no preview");
        };
        assert_eq!((big.width, big.height), (100, 50));

        // The local copy goes; the thumbnail stays.
        std::fs::remove_file(&blob).unwrap();
        assert!(matches!(thumbnail(&root, &hash), Outcome::Ready(_)));
        assert!(matches!(preview(&root, &hash, 100), Outcome::Missing));
        assert!(matches!(thumbnail(&root, &"cd".repeat(32)), Outcome::Missing));

        // Not an image, whatever its name.
        let other = "ef".repeat(32);
        let text = repo::blob_path_in(&root, &other);
        std::fs::create_dir_all(text.parent().unwrap()).unwrap();
        std::fs::write(&text, "just text").unwrap();
        assert!(matches!(thumbnail(&root, &other), Outcome::NotAnImage));
        std::fs::remove_dir_all(&root).ok();
    }
}
