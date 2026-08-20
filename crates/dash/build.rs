use std::path::{Path, PathBuf};

fn main() {
    println!("cargo:rerun-if-changed=../../web/dist");
    println!("cargo:rerun-if-changed=fallback-web");
    let dest = PathBuf::from("web-dist");
    let src = PathBuf::from("../../web/dist");
    let fallback = PathBuf::from("fallback-web");
    let _ = std::fs::remove_dir_all(&dest);
    std::fs::create_dir_all(&dest).expect("web-dist");
    if src.join("index.html").is_file() {
        copy_dir(&src, &dest);
    } else {
        copy_dir(&fallback, &dest);
    }
}

fn copy_dir(src: &Path, dest: &Path) {
    let entries = match std::fs::read_dir(src) {
        Ok(e) => e,
        Err(_) => return,
    };
    for entry in entries.flatten() {
        let from = entry.path();
        let to = dest.join(entry.file_name());
        if from.is_dir() {
            let _ = std::fs::create_dir_all(&to);
            copy_dir(&from, &to);
        } else {
            let _ = std::fs::copy(&from, &to);
        }
    }
}
