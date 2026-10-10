//! Windows-only: embed the application icon and version info into the exe.
//! A no-op for every other target (decided via CARGO_CFG_TARGET_OS so that
//! cross builds behave correctly too).

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=../../assets/icons/md-viewer.ico");
    println!("cargo:rerun-if-changed=Cargo.toml");

    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }
    // Developer escape hatch for type-checking the Windows build from Linux, where no
    // resource compiler exists. Never set in CI or release builds.
    println!("cargo:rerun-if-env-changed=MDV_SKIP_WINRES");
    if std::env::var_os("MDV_SKIP_WINRES").is_some() {
        println!(
            "cargo:warning=MDV_SKIP_WINRES set: building WITHOUT the Windows icon/version resource"
        );
        return;
    }
    windows_resources();
}

/// "0.1.0-beta2" -> (0,1,0,0): numeric core only, prerelease/build metadata dropped.
fn numeric_version(v: &str) -> u64 {
    let core = v.split(['-', '+']).next().unwrap_or("0.0.0");
    let mut parts = core
        .split('.')
        .map(|p| p.parse::<u64>().unwrap_or(0).min(0xFFFF));
    let mut next = || parts.next().unwrap_or(0);
    let (a, b, c) = (next(), next(), next());
    (a << 48) | (b << 32) | (c << 16)
}

fn windows_resources() {
    let version = std::env::var("CARGO_PKG_VERSION").unwrap_or_else(|_| "0.0.0".into());
    let num = numeric_version(&version);
    let mut res = winresource::WindowsResource::new();
    res.set_icon("../../assets/icons/md-viewer.ico");
    res.set("ProductName", "md-viewer");
    res.set("FileDescription", "Markdown viewer");
    res.set("LegalCopyright", "BSD-2-Clause");
    res.set("OriginalFilename", "md-viewer.exe");
    res.set("InternalName", "md-viewer");
    res.set("ProductVersion", &version);
    res.set("FileVersion", &version);
    res.set_version_info(winresource::VersionInfo::PRODUCTVERSION, num);
    res.set_version_info(winresource::VersionInfo::FILEVERSION, num);
    if let Err(e) = res.compile() {
        // A silently icon-less exe is worse than a failed build.
        panic!("failed to embed Windows resources: {e}");
    }
}
