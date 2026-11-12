use std::path::PathBuf;

fn main() {
    let target = std::env::var("TARGET").unwrap_or_default();
    let name = if target.contains("windows") {
        if target.contains("aarch64") {
            "lspayload-win-arm64.zip"
        } else {
            "lspayload.zip"
        }
    } else if target.contains("darwin") {
        "lspayload-mac.zip"
    } else if target.contains("aarch64") {
        "lspayload-linux-arm64.zip"
    } else {
        "lspayload-linux.zip"
    };
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let file = manifest.join("..").join("payload").join(name);
    let file = file.canonicalize().unwrap_or(file);
    println!("cargo:rerun-if-changed={}", file.display());
    println!("cargo:rustc-env=LINKSYNC_PAYLOAD={}", file.display());
    tauri_build::build()
}
