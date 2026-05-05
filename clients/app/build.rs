fn main() {
    if std::env::var_os("CARGO_FEATURE_NATIVE").is_some()
        && std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("ios")
    {
        println!("cargo:rerun-if-changed=src/ios_mobile.m");
        cc::Build::new()
            .file("src/ios_mobile.m")
            .flag("-fobjc-arc")
            .compile("intercom_ios_mobile");

        println!("cargo:rustc-link-lib=framework=AVFoundation");
        println!("cargo:rustc-link-lib=framework=Foundation");
        println!("cargo:rustc-link-lib=framework=UIKit");
    }

    if std::env::var_os("CARGO_FEATURE_NATIVE").is_some() {
        tauri_build::build();
    }
}
