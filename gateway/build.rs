//! Embeds every file under `assets/web` into the binary.
//!
//! The dashboard is plain HTML/CSS/ES modules with no build step; this script
//! only generates the table `WEB_ASSETS: &[(&str, &str)]` of
//! (relative path, contents) that `router.rs` serves from memory.

use std::{
    env, fs,
    path::{Path, PathBuf},
};

fn collect(dir: &Path, base: &Path, out: &mut Vec<String>) {
    let mut entries: Vec<_> = fs::read_dir(dir)
        .unwrap_or_else(|e| panic!("cannot read {}: {e}", dir.display()))
        .flatten()
        .collect();
    entries.sort_by_key(|e| e.file_name());
    for entry in entries {
        let path = entry.path();
        if path.is_dir() {
            collect(&path, base, out);
        } else if let Ok(rel) = path.strip_prefix(base) {
            out.push(rel.to_string_lossy().replace('\\', "/"));
        }
    }
}

fn main() {
    let manifest = PathBuf::from(env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR"));
    let web = manifest.join("../assets/web");
    println!("cargo:rerun-if-changed={}", web.display());
    println!("cargo:rerun-if-changed=build.rs");

    let mut files = Vec::new();
    collect(&web, &web, &mut files);

    let root = manifest.to_string_lossy().replace('\\', "/");
    let mut src = String::from("pub static WEB_ASSETS: &[(&str, &str)] = &[\n");
    for rel in &files {
        let abs = format!("{root}/../assets/web/{rel}");
        src.push_str(&format!("    ({rel:?}, include_str!({abs:?})),\n"));
    }
    src.push_str("];\n");

    let out = PathBuf::from(env::var("OUT_DIR").expect("OUT_DIR")).join("web_assets.rs");
    fs::write(out, src).expect("write web_assets.rs");
}
