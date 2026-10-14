//! Checks of the macOS packaging scripts: the Info.plist `scripts/bundle.py` writes passes
//! `plutil -lint` and has the keys the app needs, and the bundler's unit tests pass.
#![cfg(target_os = "macos")]

use std::path::Path;
use std::process::Command;

fn root() -> &'static Path {
    Path::new(env!("CARGO_MANIFEST_DIR"))
}

fn plist_value(plist: &Path, key: &str) -> String {
    let out = Command::new("plutil")
        .args(["-extract", key, "raw", "-o", "-"])
        .arg(plist)
        .output()
        .expect("running plutil");
    assert!(out.status.success(), "{key} missing from Info.plist");
    String::from_utf8(out.stdout).unwrap().trim().to_string()
}

#[test]
fn info_plist_lints_and_has_the_app_keys() {
    let dir = tempfile::tempdir().unwrap();
    let plist = dir.path().join("Info.plist");
    let status = Command::new("python3")
        .arg(root().join("scripts/bundle.py"))
        .arg("--info-plist")
        .arg(&plist)
        .status()
        .expect("running scripts/bundle.py");
    assert!(status.success());

    let lint = Command::new("plutil")
        .arg("-lint")
        .arg(&plist)
        .output()
        .unwrap();
    assert!(
        lint.status.success(),
        "plutil -lint: {}",
        String::from_utf8_lossy(&lint.stdout)
    );

    assert_eq!(
        plist_value(&plist, "CFBundleIdentifier"),
        "com.gylleus.stronghold"
    );
    assert_eq!(plist_value(&plist, "CFBundleName"), "Stronghold");
    assert_eq!(plist_value(&plist, "CFBundleDisplayName"), "Stronghold");
    assert_eq!(plist_value(&plist, "CFBundleExecutable"), "stronghold");
    assert_eq!(
        plist_value(&plist, "CFBundleShortVersionString"),
        env!("CARGO_PKG_VERSION")
    );
    assert_eq!(
        plist_value(&plist, "CFBundleVersion"),
        env!("CARGO_PKG_VERSION")
    );
    assert_eq!(plist_value(&plist, "NSHighResolutionCapable"), "true");
    assert_eq!(
        plist_value(&plist, "LSApplicationCategoryType"),
        "public.app-category.developer-tools"
    );
    assert!(!plist_value(&plist, "LSMinimumSystemVersion").is_empty());
    assert!(plist_value(&plist, "NSMicrophoneUsageDescription").contains("talk-to-the-King"));
}

/// The bundle id the notifier checks for is the one the bundle gets.
#[test]
fn bundle_id_matches_the_notifier() {
    let script = std::fs::read_to_string(root().join("scripts/bundle.py")).unwrap();
    let expected = format!("BUNDLE_ID = \"{}\"", stronghold::notify::native::BUNDLE_ID);
    assert!(script.contains(&expected), "bundle.py lacks {expected}");
}

#[test]
fn app_icon_is_an_icns() {
    let icns = std::fs::read(root().join("packaging/Stronghold.icns")).unwrap();
    assert_eq!(&icns[..4], b"icns");
}

#[test]
fn bundler_unit_tests_pass() {
    let out = Command::new("python3")
        .env("PYTHONDONTWRITEBYTECODE", "1")
        .args(["-m", "unittest", "discover", "-s"])
        .arg(root().join("scripts"))
        .args(["-p", "test_*.py"])
        .output()
        .expect("running python3");
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}
