//! Teste do caminho nativo da verificação de capacidade com arquivos reais numa pasta
//! temporária (não toca em disco físico nem em arquivos do usuário).
//!
//! Ignorado por padrão porque grava ~64 MiB. Rode com:
//! `cargo test --test capacity_win -- --ignored`

#![cfg(windows)]

use diskdoctor::capacity::{CapacityConfig, CapacityVerdict, NoopCapacityObserver, run_capacity_test};
use diskdoctor::platform::CapacityDir;
use std::sync::atomic::AtomicBool;

#[test]
#[ignore = "grava 64 MiB numa pasta temporária"]
fn capacity_dir_writes_verifies_and_cleans_up() {
    let parent = tempfile::tempdir().unwrap();
    let mut dir = CapacityDir::create(parent.path(), 1 << 20).unwrap();
    let test_dir = dir.dir().to_path_buf();
    assert!(test_dir.starts_with(parent.path()));

    let cfg = CapacityConfig { total_bytes: 64 << 20, file_size: 16 << 20, seed: 42, ..Default::default() };
    let m = run_capacity_test(&mut dir, &cfg, &mut NoopCapacityObserver, &AtomicBool::new(false));
    assert_eq!(m.verdict, CapacityVerdict::Genuine, "{m:?}");
    assert_eq!(m.ok_bytes, 64 << 20);
    assert_eq!(m.files_created, 4);
    assert_eq!(std::fs::read_dir(&test_dir).unwrap().count(), 4);

    assert!(dir.cleanup().is_empty());
    assert!(!test_dir.exists(), "pasta de teste removida");
    // A pasta-mãe (que não é nossa) continua lá.
    assert!(parent.path().exists());
}

#[test]
#[ignore = "cria uma pasta temporária"]
fn capacity_dir_never_reuses_existing_names() {
    let parent = tempfile::tempdir().unwrap();
    let a = CapacityDir::create(parent.path(), 1 << 20).unwrap();
    // Mesmo segundo → mesmo nome → precisa falhar em vez de reutilizar a pasta.
    let b = CapacityDir::create(parent.path(), 1 << 20);
    if let Ok(b) = &b {
        assert_ne!(a.dir(), b.dir());
    }
}
