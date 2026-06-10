//! Staging area for bitmaps pasted into the tray UI. A pasted image has no path, but the file
//! pipeline ships paths, so the bitmap is PNG-encoded into `<cache>/outbox/` and sent like a
//! dropped file. The staged file must outlive the publish — recipients pull it through the
//! regular transfer whenever they press the receive hotkey — so cleanup is age-based on the
//! next staging rather than right after the send.

use anyhow::Context;
use image::ImageEncoder;
use std::path::{Path, PathBuf};
use std::time::Duration;

/// Staged files older than this are pruned; matches the longest plausible "sent it, the other
/// machine picks it up later" window before the clip itself has long expired.
const MAX_STAGED_AGE: Duration = Duration::from_secs(24 * 60 * 60);

/// Encode a pasted RGBA bitmap as a PNG under the cache dir and return its path.
pub fn stage_pasted_image_png(rgba: &[u8], width: u32, height: u32) -> anyhow::Result<PathBuf> {
    let expected_len = (width as usize) * (height as usize) * 4;
    anyhow::ensure!(
        rgba.len() == expected_len,
        "pasted image buffer is {} bytes, expected {} for {}x{} rgba",
        rgba.len(),
        expected_len,
        width,
        height,
    );

    let dir = crate::config::default_cache_dir().join("outbox");
    std::fs::create_dir_all(&dir)
        .with_context(|| format!("failed to create outbox dir {}", dir.display()))?;
    prune_stale(&dir);

    let name = format!(
        "pasted-{}.png",
        chrono::Local::now().format("%Y%m%d-%H%M%S%.3f")
    );
    let path = dir.join(name);
    write_png(&path, rgba, width, height)?;
    Ok(path)
}

fn write_png(path: &Path, rgba: &[u8], width: u32, height: u32) -> anyhow::Result<()> {
    let file = std::fs::File::create(path)
        .with_context(|| format!("failed to create {}", path.display()))?;
    image::codecs::png::PngEncoder::new(std::io::BufWriter::new(file))
        .write_image(rgba, width, height, image::ExtendedColorType::Rgba8)
        .context("failed to encode pasted image as png")?;
    Ok(())
}

/// Best-effort removal of staged files past `MAX_STAGED_AGE`.
fn prune_stale(dir: &Path) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let stale = entry
            .metadata()
            .and_then(|meta| meta.modified())
            .ok()
            .and_then(|modified| modified.elapsed().ok())
            .is_some_and(|age| age > MAX_STAGED_AGE);
        if stale {
            let _ = std::fs::remove_file(entry.path());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn writes_a_decodable_png() {
        let (width, height) = (3u32, 2u32);
        let rgba: Vec<u8> = (0..width * height * 4).map(|i| i as u8).collect();
        let path = std::env::temp_dir().join("airpaste-outbox-test.png");
        write_png(&path, &rgba, width, height).expect("encode png");

        let decoded = image::open(&path).expect("decode png").to_rgba8();
        assert_eq!(decoded.dimensions(), (width, height));
        assert_eq!(decoded.into_raw(), rgba);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn rejects_mismatched_buffer_len() {
        assert!(stage_pasted_image_png(&[0u8; 10], 2, 2).is_err());
    }
}
