use std::io::{Cursor, Write};

use anyhow::{Context, Result};
use flate2::{write::GzEncoder, Compression};
use image::{DynamicImage, ImageBuffer, ImageFormat, Rgba};
use tar::{Builder as TarBuilder, Header};
use zip::{write::SimpleFileOptions, ZipWriter};

use crate::model::Artifact;

pub fn encode(artifacts: &[Artifact]) -> Result<usize> {
    let json = serde_json::to_vec(artifacts)?;

    let mut gzip = GzEncoder::new(Vec::new(), Compression::best());
    gzip.write_all(&json)?;
    let gzip = gzip.finish()?;
    let zstd = zstd::stream::encode_all(Cursor::new(&json), 9)?;

    let mut zip = ZipWriter::new(Cursor::new(Vec::new()));
    zip.start_file(
        "artifacts.json",
        SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated),
    )?;
    zip.write_all(&json)?;
    let zip = zip.finish()?.into_inner();

    let mut tar = TarBuilder::new(Vec::new());
    let mut header = Header::new_gnu();
    header.set_size(json.len() as u64);
    header.set_mode(0o644);
    header.set_cksum();
    tar.append_data(&mut header, "artifacts.json", Cursor::new(&json))?;
    let tar = tar.into_inner()?;

    let pixels = ImageBuffer::from_fn(96, 96, |x, y| {
        let byte = json[(x as usize * 97 + y as usize) % json.len()];
        Rgba([byte, byte.rotate_left(2), byte.rotate_left(5), 255])
    });
    let mut png = Cursor::new(Vec::new());
    DynamicImage::ImageRgba8(pixels)
        .write_to(&mut png, ImageFormat::Png)
        .context("encode audit thumbnail")?;

    Ok(gzip.len() + zstd.len() + zip.len() + tar.len() + png.into_inner().len())
}
