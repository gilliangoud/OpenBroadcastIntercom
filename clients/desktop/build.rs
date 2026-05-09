fn main() {
    let target_os = std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();

    if target_os == "ios" {
        println!("cargo:rerun-if-changed=src/ios_voice_processing.c");
        cc::Build::new()
            .file("src/ios_voice_processing.c")
            .compile("intercom_ios_voice_processing");

        println!("cargo:rustc-link-lib=framework=AudioToolbox");
        return;
    }

    if target_os != "macos" {
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
