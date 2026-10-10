//! Real install/doctor/uninstall cycle inside a throwaway HOME.
//! Usage: cargo run -p md-install --example dryrun
//! Refuses to run against anything but a fresh temp dir.
#[cfg(unix)]
fn main() {
    use md_install::{unix::*, FixedPrompter, InstallOptions, UninstallOptions};
    let td = std::env::temp_dir().join(format!("md-install-dryrun-{}", std::process::id()));
    let home = td.join("home");
    std::fs::create_dir_all(&home).unwrap();
    // Make sure nothing below can see the real HOME.
    std::env::set_var("HOME", &home);
    for v in ["XDG_DATA_HOME", "XDG_CONFIG_HOME", "XDG_BIN_HOME"] {
        std::env::remove_var(v);
    }
    let env = UnixEnv::from_env().unwrap();
    assert!(env.bin().starts_with(&td));
    let mime = SystemMime::new(&env);
    let opts = InstallOptions { yes: true, no_assoc: false };
    println!("{}", install_with(&env, &mime, &mut FixedPrompter(false), &opts).unwrap());
    println!("{}", doctor_with(&env, &mime, &SystemProbe));
    println!("{}", uninstall_with(&env, &mime, &UninstallOptions::default()).unwrap());
    let _ = std::fs::remove_dir_all(&td);
}
#[cfg(not(unix))]
fn main() {}
