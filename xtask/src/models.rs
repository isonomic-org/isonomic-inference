//! `cargo xtask models` and `cargo xtask fetch`.

use std::path::PathBuf;

use isonomic_inference_core::ModelRef;
use isonomic_model_library::{ensure_model, DownloadProgress, Manifest};

use crate::args::Args;

/// Print the manifest.
pub fn list() -> Result<(), String> {
    let manifest = Manifest::builtin();
    let default_chat = &manifest.default_chat().meta.id;
    let default_embed = manifest.default_embeddings().map(|e| &e.meta.id);
    for entry in manifest.entries() {
        let meta = &entry.meta;
        let caps: Vec<&str> = meta.capabilities.iter().map(|c| c.as_str()).collect();
        let mut notes = Vec::new();
        if &meta.id == default_chat || Some(&meta.id) == default_embed {
            notes.push("default");
        }
        if entry.draft {
            notes.push("hash not yet confirmed by a download");
        }
        println!(
            "{}\n    {} · {:.2} GB · {} · {}{}",
            meta.id,
            meta.name,
            meta.size_bytes as f64 / 1e9,
            caps.join(", "),
            meta.license.name,
            if notes.is_empty() {
                String::new()
            } else {
                format!(" · {}", notes.join(", "))
            }
        );
    }
    Ok(())
}

/// Download one model and verify it.
pub fn fetch(argv: &[String]) -> Result<(), String> {
    let args = Args::parse(argv, &["dir"])?;
    let [id] = args.positional.as_slice() else {
        return Err("usage: cargo xtask fetch <model-id> [--dir <dir>]".to_string());
    };
    let manifest = Manifest::builtin();
    let entry = manifest
        .get(&ModelRef::new(id.as_str()))
        .ok_or_else(|| format!("no model named {id}; run `cargo xtask models`"))?;
    let dir = PathBuf::from(args.get("dir").unwrap_or("models"));
    let spec = entry.spec();
    println!("fetching {} into {}", spec.file_name, dir.display());

    let mut last_percent = u64::MAX;
    let report = |event: DownloadProgress| match event {
        DownloadProgress::Started { total_bytes } => {
            println!("downloading {:.2} GB", total_bytes as f64 / 1e9)
        }
        DownloadProgress::Received { bytes, total_bytes } if total_bytes > 0 => {
            let percent = bytes * 100 / total_bytes;
            if percent != last_percent && percent % 10 == 0 {
                last_percent = percent;
                println!("  {percent}%");
            }
        }
        DownloadProgress::Received { .. } => {}
        DownloadProgress::Verifying => println!("verifying sha256"),
        DownloadProgress::Cached => println!("already downloaded and verified"),
        DownloadProgress::Done => println!("downloaded and verified"),
    };
    let path = crate::block_on(ensure_model(&spec, &dir, report)).map_err(|e| e.to_string())?;
    println!("{}", path.display());
    if entry.draft {
        println!(
            "note: this hash is now confirmed by a real download; \
             the entry's draft flag in crates/library/models.json can be cleared"
        );
    }
    Ok(())
}
