fn main() {
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("macos") {
        return;
    }

    println!("cargo:rerun-if-changed=src/macos_mic_mode.m");
    cc::Build::new()
        .file("src/macos_mic_mode.m")
        .flag("-fobjc-arc")
        .compile("intercom_macos_mic_mode");

    println!("cargo:rustc-link-lib=framework=AVFoundation");
    println!("cargo:rustc-link-lib=framework=Foundation");
}
