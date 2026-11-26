fn main() {
    // The target triple, for self-update's release-asset selection.
    println!(
        "cargo:rustc-env=BUILD_TARGET={}",
        std::env::var("TARGET").unwrap()
    );
    // The commit this is built from, where git knows it: the views another
    // build of the same version wrote are made again (`views.json` `build`).
    let commit = std::process::Command::new("git")
        .args(["rev-parse", "HEAD"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .filter(|c| c.len() == 40 && c.bytes().all(|b| b.is_ascii_hexdigit()))
        .unwrap_or_default();
    println!("cargo:rustc-env=BUILD_COMMIT={commit}");
}
