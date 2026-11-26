//! PNG files exchanged with the runner.

use std::fs::File;
use std::io::{BufReader, BufWriter};
use std::path::Path;

use crate::pack::MAX_SIDE;
use crate::runner::{Result, UpscaleError};

fn bad(path: &Path, reason: impl std::fmt::Display) -> UpscaleError {
    UpscaleError::BadOutput {
        file: path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default(),
        reason: reason.to_string(),
    }
}

/// Writes an 8-bit RGB PNG (fast compression: the file is read once).
pub fn write_png_rgb(path: &Path, width: u32, height: u32, rgb: &[u8]) -> Result<()> {
    let f = BufWriter::new(File::create(path)?);
    let mut enc = png::Encoder::new(f, width, height);
    enc.set_color(png::ColorType::Rgb);
    enc.set_depth(png::BitDepth::Eight);
    enc.set_compression(png::Compression::Fast);
    let mut w = enc.write_header().map_err(|e| bad(path, e))?;
    w.write_image_data(rgb).map_err(|e| bad(path, e))?;
    w.finish().map_err(|e| bad(path, e))?;
    Ok(())
}

/// Reads a PNG as 8-bit RGB (alpha, if any, is dropped; grey is expanded).
pub fn read_png_rgb(path: &Path) -> Result<(u32, u32, Vec<u8>)> {
    let f = match File::open(path) {
        Ok(f) => f,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Err(UpscaleError::MissingOutput {
                file: path
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_default(),
                stderr: String::new(),
            });
        }
        Err(e) => return Err(e.into()),
    };
    let mut dec = png::Decoder::new(BufReader::new(f));
    dec.set_transformations(png::Transformations::normalize_to_color8());
    let mut reader = dec.read_info().map_err(|e| bad(path, e))?;
    let (w, h) = (reader.info().width, reader.info().height);
    if w == 0 || h == 0 || w > MAX_SIDE || h > MAX_SIDE {
        return Err(bad(path, format!("size {w}x{h} out of range")));
    }
    let size = reader
        .output_buffer_size()
        .ok_or_else(|| bad(path, "image too large"))?;
    let mut buf = vec![0u8; size];
    let info = reader.next_frame(&mut buf).map_err(|e| bad(path, e))?;
    buf.truncate(info.buffer_size());
    let px = w as usize * h as usize;
    let rgb = match info.color_type {
        png::ColorType::Rgb => buf,
        png::ColorType::Rgba => buf
            .as_chunks::<4>()
            .0
            .iter()
            .flat_map(|p| [p[0], p[1], p[2]])
            .collect(),
        png::ColorType::Grayscale => buf.iter().flat_map(|&g| [g, g, g]).collect(),
        png::ColorType::GrayscaleAlpha => buf
            .as_chunks::<2>()
            .0
            .iter()
            .flat_map(|p| [p[0], p[0], p[0]])
            .collect(),
        png::ColorType::Indexed => return Err(bad(path, "indexed PNG not expanded")),
    };
    if rgb.len() != px * 3 {
        return Err(bad(path, "pixel data does not match the size"));
    }
    Ok((w, h, rgb))
}
