//! fake-zingod, the development `zeal-zingod` that `scripts/dev-sidecar.sh`
//! installs on Linux. check-versions.sh runs before anything compiles, so this
//! test is what checks the binary against the pin.

use std::process::{Command, Output};

const FAKE_ZINGOD: &str = env!("CARGO_BIN_EXE_fake-zingod");

fn run(args: &[&str]) -> Output {
    Command::new(FAKE_ZINGOD)
        .args(args)
        .output()
        .expect("run fake-zingod")
}

#[test]
fn reports_the_pinned_version() {
    let pin = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/../../ZINGOD_VERSION"))
        .expect("read ZINGOD_VERSION");
    for flag in ["--version", "-V"] {
        let out = run(&[flag]);
        assert!(out.status.success(), "{flag}: {out:?}");
        assert_eq!(
            String::from_utf8(out.stdout).expect("UTF-8"),
            format!("zingod {}\n", pin.trim()),
            "{flag}"
        );
    }
}

#[test]
fn generates_a_config_file() {
    let dir = tempfile::tempdir().expect("temporary directory");
    let path = dir.path().join("zingod.toml");
    let path = path.to_str().expect("UTF-8 path");
    for flag in ["-o", "--output"] {
        let out = run(&["generate-config", flag, path]);
        assert!(out.status.success(), "{flag}: {out:?}");
        let config = std::fs::read_to_string(path).expect("the generated file");
        assert!(config.contains("network = \"mainnet\"\n"), "{config}");
        std::fs::remove_file(path).expect("remove the generated file");
    }
}

#[test]
fn refuses_anything_else() {
    for args in [
        &[][..],
        &["start"],
        &["-c", "zingod.toml", "start"],
        &["generate-config"],
        &["--version", "extra"],
    ] {
        let out = run(args);
        assert_eq!(out.status.code(), Some(2), "{args:?}: {out:?}");
        assert!(out.stdout.is_empty(), "{args:?}: {out:?}");
    }
}
