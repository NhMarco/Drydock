//! Writes the same payload as several ZIP variants so the Windows Explorer ("zipfldr") extractor
//! can be tested against each. Explorer chokes on some deflate streams that are otherwise valid.
//!
//! Usage: cargo run -p drydock-core --example zip_variants -- <input file> <output dir>

use std::io::Write;
use std::path::PathBuf;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args().skip(1);
    let input = PathBuf::from(args.next().ok_or("missing input file")?);
    let out_dir = PathBuf::from(args.next().ok_or("missing output dir")?);
    std::fs::create_dir_all(&out_dir)?;

    let payload = std::fs::read(&input)?;
    let entry = input
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or("input has no file name")?
        .to_owned();
    println!("payload: {} ({} bytes)", entry, payload.len());

    // The production path, so this doubles as a regression probe for the real exporter.
    let produced = drydock_core::emu_template::zip_files(&[(entry.clone(), payload.clone())])?;
    std::fs::write(out_dir.join("production.zip"), &produced)?;
    println!("{:<16} -> {} bytes", "production", produced.len());

    let variants: Vec<(&str, zip::CompressionMethod, Option<i64>)> = vec![
        ("deflate_default", zip::CompressionMethod::Deflated, None),
        ("deflate_l1", zip::CompressionMethod::Deflated, Some(1)),
        ("deflate_l9", zip::CompressionMethod::Deflated, Some(9)),
        ("stored", zip::CompressionMethod::Stored, None),
    ];

    for (label, method, level) in variants {
        let mut buffer = std::io::Cursor::new(Vec::new());
        let mut zip = zip::ZipWriter::new(&mut buffer);
        let mut options: zip::write::FileOptions<()> =
            zip::write::FileOptions::default().compression_method(method);
        if let Some(level) = level {
            options = options.compression_level(Some(level));
        }
        zip.start_file(entry.clone(), options)?;
        zip.write_all(&payload)?;
        zip.finish()?;
        let bytes = buffer.into_inner();
        let path = out_dir.join(format!("{label}.zip"));
        std::fs::write(&path, &bytes)?;
        println!("{label:<16} -> {} bytes", bytes.len());
    }
    Ok(())
}
