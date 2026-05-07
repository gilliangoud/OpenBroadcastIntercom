fn main() {
    let native = std::env::var_os("CARGO_FEATURE_NATIVE").is_some();
    let target_os = std::env::var("CARGO_CFG_TARGET_OS").ok();

    if native && target_os.as_deref() == Some("android") {
        link_android_cpp_runtime();
    }

    if native && target_os.as_deref() == Some("ios") {
        println!("cargo:rerun-if-changed=src/ios_mobile.m");
        cc::Build::new()
            .file("src/ios_mobile.m")
            .flag("-fobjc-arc")
            .compile("intercom_ios_mobile");

        println!("cargo:rustc-link-lib=framework=AVFoundation");
        println!("cargo:rustc-link-lib=framework=Foundation");
        println!("cargo:rustc-link-lib=framework=UIKit");
    }

    if native {
        tauri_build::build();
    }
}

fn link_android_cpp_runtime() {
    if let Some(runtime) = android_ndk_cxx_runtime() {
        copy_android_cpp_shared(&runtime);

        // cpal's Android backend pulls in Oboe C++ objects. Link the exact NDK
        // shared libc++ runtime without adding the generic NDK lib directory to
        // the linker search path; that directory also contains static Bionic
        // libc.a objects that crash if pulled into the app dylib.
        println!("cargo:rustc-link-arg=-Wl,--no-as-needed");
        println!(
            "cargo:rustc-link-arg={}",
            runtime.lib_dir.join("libc++_shared.so").display()
        );
    }
}

struct AndroidCxxRuntime {
    lib_dir: std::path::PathBuf,
    abi: &'static str,
}

fn android_ndk_cxx_runtime() -> Option<AndroidCxxRuntime> {
    let ndk = std::env::var_os("ANDROID_NDK_HOME")
        .or_else(|| std::env::var_os("ANDROID_NDK_ROOT"))
        .or_else(|| std::env::var_os("ANDROID_NDK"))
        .or_else(|| std::env::var_os("NDK_HOME"))?;
    let (arch, abi) = match std::env::var("CARGO_CFG_TARGET_ARCH").ok()?.as_str() {
        "aarch64" => ("aarch64-linux-android", "arm64-v8a"),
        "arm" => ("arm-linux-androideabi", "armeabi-v7a"),
        "x86" => ("i686-linux-android", "x86"),
        "x86_64" => ("x86_64-linux-android", "x86_64"),
        _ => return None,
    };
    let prebuilt = std::path::Path::new(&ndk)
        .join("toolchains")
        .join("llvm")
        .join("prebuilt");
    let host = std::fs::read_dir(prebuilt)
        .ok()?
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .find(|path| path.join("sysroot").exists())?;
    Some(AndroidCxxRuntime {
        lib_dir: host.join("sysroot").join("usr").join("lib").join(arch),
        abi,
    })
}

fn copy_android_cpp_shared(runtime: &AndroidCxxRuntime) {
    let src = runtime.lib_dir.join("libc++_shared.so");
    let out_dir = std::path::Path::new("gen")
        .join("android")
        .join("app")
        .join("src")
        .join("main")
        .join("jniLibs")
        .join(runtime.abi);
    let dst = out_dir.join("libc++_shared.so");

    if let Err(err) = std::fs::create_dir_all(&out_dir) {
        println!("cargo:warning=failed to create Android jniLibs dir: {err}");
        return;
    }
    if std::fs::symlink_metadata(&dst)
        .map(|metadata| metadata.file_type().is_symlink())
        .unwrap_or(false)
    {
        if let Err(err) = std::fs::remove_file(&dst) {
            println!(
                "cargo:warning=failed to remove stale Android libc++_shared.so symlink {}: {err}",
                dst.display()
            );
            return;
        }
    }
    if let Err(err) = std::fs::copy(&src, &dst) {
        println!(
            "cargo:warning=failed to copy Android libc++_shared.so from {} to {}: {err}",
            src.display(),
            dst.display()
        );
    }
}
