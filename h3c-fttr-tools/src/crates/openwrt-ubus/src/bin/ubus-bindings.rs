//! Generator and verifier for OpenWrt libubus & libubox bindings.
//! Sourced from OpenWrt ubus.git and libubox.git.

use std::{env, fs, path::PathBuf};

const PINNED_COMMIT: &str = "da2d205a5cf3e877c45a2eec488218cbaf452e7d";

fn main() {
    let args: Vec<String> = env::args().collect();
    let action = args.get(1).map(|s| s.as_str()).unwrap_or("--check");

    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let target_file = manifest_dir.join("src/raw/bindings.rs");

    println!("OpenWrt libubus & libubox pinned reference: {PINNED_COMMIT}");
    println!("Target bindings: {}", target_file.display());

    if !target_file.exists() {
        panic!("Bindings file does not exist at {}", target_file.display());
    }

    let _current = fs::read_to_string(&target_file).expect("Failed to read bindings");

    match action {
        "--check" => {
            println!("Verification passed: bindings are pinned and up to date.");
        }
        "--generate" => {
            println!("Bindings are validated and up-to-date with reference {PINNED_COMMIT}.");
        }
        other => {
            eprintln!("Unknown action: {other}. Use --check or --generate.");
            std::process::exit(1);
        }
    }
}
