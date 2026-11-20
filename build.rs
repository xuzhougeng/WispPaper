//! Embed every file under assets/ into the binary (see src/assets.rs).

use std::fmt::Write;
use std::path::Path;

fn main() {
    // Read at run time: a compile-time path goes stale when the checkout moves.
    let root = Path::new(&std::env::var("CARGO_MANIFEST_DIR").unwrap()).join("assets");
    println!("cargo:rerun-if-changed={}", root.display());
    let mut files = vec![];
    collect(&root, &root, &mut files);
    files.sort();
    let mut out = String::from("pub static FILES: &[(&str, &[u8])] = &[\n");
    for (rel, abs) in files {
        writeln!(out, "    ({rel:?}, include_bytes!({abs:?})),").unwrap();
    }
    out.push_str("];\n");
    let target = Path::new(&std::env::var("OUT_DIR").unwrap()).join("assets.rs");
    std::fs::write(target, out).unwrap();

    // The executable's icon and version info (Explorer, taskbar, window corner).
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        let mut resource = winresource::WindowsResource::new();
        resource.set_icon("assets/branding/app-icon.ico").set("ProductName", "Wisp Study").set("FileDescription", "Wisp Study");
        resource.compile().expect("Windows resource compilation failed");
    }
}

fn collect(root: &Path, dir: &Path, files: &mut Vec<(String, String)>) {
    for entry in std::fs::read_dir(dir).unwrap().flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect(root, &path, files);
        } else {
            let rel = path.strip_prefix(root).unwrap().to_string_lossy().replace('\\', "/");
            // The app icon goes into the executable as a resource; only the PNG is also an asset (设置 › 关于).
            if (rel.starts_with("branding/") && rel != "branding/app-icon.png") || rel.starts_with("icons/LICENSE") { continue }
            files.push((rel, path.to_string_lossy().to_string()));
        }
    }
}
