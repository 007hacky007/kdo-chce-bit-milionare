//! `export-frames`: dumps a Smacker file as PNGs for local inspection.

use std::fs::{self, File};
use std::io::BufWriter;
use std::path::Path;

use formats::smacker::Smacker;
use gamedata::GameData;

pub fn export_frames(gd: &GameData, name: &str, out_dir: &Path) -> Result<usize, String> {
    let bytes = gd.read(name).map_err(|e| e.to_string())?;
    let smk = Smacker::parse(bytes).map_err(|e| format!("{name}: {e}"))?;
    let h = smk.header().clone();
    fs::create_dir_all(out_dir).map_err(|e| format!("{}: {e}", out_dir.display()))?;
    let mut changed = Vec::new();
    let mut dec = smk.decoder();
    while let Some(f) = dec.next_frame().map_err(|e| format!("{name}: {e}"))? {
        let rgba: Vec<u8> = f
            .pixels
            .iter()
            .flat_map(|&p| {
                let [r, g, b] = f.palette[p as usize];
                [r, g, b, 255]
            })
            .collect();
        let path = out_dir.join(format!("{:04}.png", f.index));
        let file = File::create(&path).map_err(|e| format!("{}: {e}", path.display()))?;
        let mut enc = png::Encoder::new(BufWriter::new(file), h.width, h.height);
        enc.set_color(png::ColorType::Rgba);
        enc.set_depth(png::BitDepth::Eight);
        enc.write_header()
            .and_then(|mut w| w.write_image_data(&rgba))
            .map_err(|e| format!("{}: {e}", path.display()))?;
        changed.push(f.palette_changed);
    }
    let meta = serde_json::json!({
        "width": h.width,
        "height": h.height,
        "frame_duration_us": h.frame_duration_us,
        "frames": changed.iter().map(|c| serde_json::json!({ "palette_changed": c })).collect::<Vec<_>>(),
    });
    let path = out_dir.join("frames.json");
    fs::write(
        &path,
        serde_json::to_vec_pretty(&meta).map_err(|e| e.to_string())?,
    )
    .map_err(|e| format!("{}: {e}", path.display()))?;
    Ok(changed.len())
}
