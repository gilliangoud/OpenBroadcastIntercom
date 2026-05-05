use std::path::Path;
use std::process::Command;

fn main() {
    for name in [
        "INTERCOM_RELEASE_TAG",
        "INTERCOM_GIT_SHA",
        "INTERCOM_BUILD_TIMESTAMP",
        "INTERCOM_GIT_DIRTY",
    ] {
        println!("cargo:rerun-if-env-changed={name}");
    }

    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("common crate has workspace parent");
    if let Ok(tag) = std::env::var("INTERCOM_RELEASE_TAG") {
        println!("cargo:rustc-env=INTERCOM_RELEASE_TAG={tag}");
    }
    if let Ok(timestamp) = std::env::var("INTERCOM_BUILD_TIMESTAMP") {
        println!("cargo:rustc-env=INTERCOM_BUILD_TIMESTAMP={timestamp}");
    }

    let git_sha = std::env::var("INTERCOM_GIT_SHA")
        .ok()
        .or_else(|| git_output(root, &["rev-parse", "--short=12", "HEAD"]));
    if let Some(git_sha) = git_sha {
        println!("cargo:rustc-env=INTERCOM_GIT_SHA={git_sha}");
    }

    let dirty = std::env::var("INTERCOM_GIT_DIRTY")
        .ok()
        .unwrap_or_else(|| if git_dirty(root) { "1" } else { "0" }.to_string());
    println!("cargo:rustc-env=INTERCOM_GIT_DIRTY={dirty}");
}

fn git_output(root: &Path, args: &[&str]) -> Option<String> {
    let output = Command::new("git")
        .args(args)
        .current_dir(root)
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let value = String::from_utf8(output.stdout).ok()?;
    let value = value.trim();
    (!value.is_empty()).then(|| value.to_string())
}

fn git_dirty(root: &Path) -> bool {
    Command::new("git")
        .args(["status", "--porcelain", "--untracked-files=normal"])
        .current_dir(root)
        .output()
        .map(|output| output.status.success() && !output.stdout.is_empty())
        .unwrap_or(false)
}
