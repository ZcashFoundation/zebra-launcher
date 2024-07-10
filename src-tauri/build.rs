//! Build script for zebra-app

fn main() {
    let zebrad_binary_path = std::env::var_os("CARGO_BIN_FILE_ZEBRAD_zebrad")
        .expect("zebrad binary should be compiled as an artifact dependency");

    let target_platform = std::env::var_os("TARGET")
        .expect("must have a build target")
        .into_string()
        .expect("target should successfully convert into string");

    let zebrad_external_bin_dir_path = std::env::current_dir()
        .expect("should have a current directory")
        .join("binaries");

    std::fs::create_dir_all(&zebrad_external_bin_dir_path)
        .expect("must have permission to create binaries directory if it missing");

    let zebrad_external_bin_path =
        zebrad_external_bin_dir_path.join(format!("zebrad-{target_platform}"));

    if !std::fs::exists(&zebrad_external_bin_path).expect("must check if file exists") {
        std::fs::copy(zebrad_binary_path, zebrad_external_bin_path)
            .expect("build must copy zebrad into external binary directory");
    }

    tauri_build::build()
}
