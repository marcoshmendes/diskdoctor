//! Test of the native Capacity Verification path with real files in a temporary folder
//! (touches no physical drive and no user file).
//!
//! Ignored by default because it writes ~64 MiB. Run it with:
//! `cargo test --test capacity_win -- --ignored`

#![cfg(windows)]

use diskdoctor::capacity::{CapacityConfig, CapacityVerdict, NoopCapacityObserver, run_capacity_test};
use diskdoctor::platform::CapacityDir;
use std::sync::atomic::AtomicBool;

#[test]
#[ignore = "writes 64 MiB to a temporary folder"]
fn capacity_dir_writes_verifies_and_cleans_up() {
    let parent = tempfile::tempdir().unwrap();
    let mut dir = CapacityDir::create(parent.path(), 1 << 20).unwrap();
    let test_dir = dir.dir().to_path_buf();
    assert!(test_dir.starts_with(parent.path()));
    assert!(test_dir.file_name().unwrap().to_string_lossy().starts_with("diskdoctor-capacity-"));

    let cfg = CapacityConfig { total_bytes: 64 << 20, file_size: 16 << 20, seed: 42, ..Default::default() };
    let m = run_capacity_test(&mut dir, &cfg, &mut NoopCapacityObserver, &AtomicBool::new(false));
    assert_eq!(m.verdict, CapacityVerdict::Genuine, "{m:?}");
    assert_eq!(m.ok_bytes, 64 << 20);
    assert_eq!(m.files_created, 4);
    assert_eq!(std::fs::read_dir(&test_dir).unwrap().count(), 4);

    assert!(dir.cleanup().is_empty());
    assert!(!test_dir.exists(), "test folder removed");
    // The parent folder (which is not ours) is still there.
    assert!(parent.path().exists());
}

#[test]
#[ignore = "creates a temporary folder"]
fn capacity_dir_never_reuses_existing_names() {
    let parent = tempfile::tempdir().unwrap();
    let a = CapacityDir::create(parent.path(), 1 << 20).unwrap();
    // Same second → same name → must fail instead of reusing the folder.
    let b = CapacityDir::create(parent.path(), 1 << 20);
    if let Ok(b) = &b {
        assert_ne!(a.dir(), b.dir());
    }
}
