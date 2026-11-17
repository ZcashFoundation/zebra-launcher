//! A minimal stand-in for `zingod`, installed as the development sidecar
//! `zeal-zingod` on Linux by `scripts/dev-sidecar.sh`, so `tauri-build` finds
//! a file for every `externalBin` entry without building the real indexer.
//! Release bundles never ship it, and nothing in Zeal runs it yet.
//!
//! | command | behaviour |
//! |---|---|
//! | `--version`, `-V` | prints `zingod <ZINGOD_VERSION>`, the pin, and exits 0 |
//! | `generate-config -o FILE` (or `--output FILE`) | writes a small TOML to `FILE` and exits 0 |
//!
//! Anything else exits 2, as zingod does for a command it doesn't know.

use std::process::ExitCode;

/// What `generate-config` writes: two of zingod's top-level keys, at zingod's defaults.
const CONFIG: &str = "\
# Written by fake-zingod, a stand-in for zingod in Zeal's development builds.
network = \"mainnet\"
validator_client = \"legacy_rpc\"
";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    match args.as_slice() {
        ["--version" | "-V"] => {
            println!("zingod {}", launcher_types::zingod::version());
            ExitCode::SUCCESS
        }
        ["generate-config", "-o" | "--output", path] => match std::fs::write(path, CONFIG) {
            Ok(()) => {
                println!("Generated config file: {path}");
                ExitCode::SUCCESS
            }
            Err(err) => {
                eprintln!("fake-zingod: cannot write {path}: {err}");
                ExitCode::FAILURE
            }
        },
        _ => {
            eprintln!("fake-zingod: expected --version or generate-config -o FILE");
            ExitCode::from(2)
        }
    }
}
