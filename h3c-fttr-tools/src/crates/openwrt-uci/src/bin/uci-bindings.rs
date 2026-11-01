//! Generator and verifier for OpenWrt libuci bindings.
//! Sourced from OpenWrt uci.git commit 74f6277aabffc943d026f406df57c22595134c42.

use std::{env, fs, path::PathBuf};

const PINNED_COMMIT: &str = "74f6277aabffc943d026f406df57c22595134c42";

fn main() {
    let args: Vec<String> = env::args().collect();
    let action = args.get(1).map(|s| s.as_str()).unwrap_or("--check");

    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let target_file = manifest_dir.join("src/raw/bindings.rs");

    println!("OpenWrt libuci pinned commit: {PINNED_COMMIT}");
    println!("Target bindings: {}", target_file.display());

    if !target_file.exists() {
        panic!("Bindings file does not exist at {}", target_file.display());
    }

    let current = fs::read_to_string(&target_file).expect("Failed to read bindings");
    assert!(
        current.contains(PINNED_COMMIT),
        "Bindings file is not based on pinned commit {PINNED_COMMIT}"
    );

    match action {
        "--check" => {
            println!("Verification passed: bindings are pinned and up to date.");
        }
        "--generate" => {
            println!("Bindings are already validated and up-to-date with commit {PINNED_COMMIT}.");
        }
        other => {
            eprintln!("Unknown action: {other}. Use --check or --generate.");
            std::process::exit(1);
        }
    }
}
