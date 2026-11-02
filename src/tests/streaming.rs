use super::{convergence_fixture, make_test_tree_writable, make_test_writable};
use crate::{copy_verified_file, export_file, export_file_manifest};
use sha2::{Digest, Sha256};
use std::{
    fs,
    io::{self, Write},
};

#[test]
fn file_export_is_offline_exact_and_atomic_on_corruption_and_missing_paths() {
    let (root, _, lock) = convergence_fixture();
    let store = root.join("store");
    let manifest = export_file_manifest(&lock, &store).unwrap();
    let wanted = manifest
        .files
        .iter()
        .find(|file| file.tds_path.ends_with("alpha.sty"))
        .unwrap();
    let output = root.join("export.sty");
    let metadata = export_file(&lock, &store, &wanted.tds_path, &output).unwrap();
    assert_eq!(&metadata, wanted);
    let bytes = fs::read(&output).unwrap();
    assert_eq!(
        metadata.digest,
        format!("sha256:{}", hex::encode(Sha256::digest(&bytes)))
    );
    let mut stream = Vec::new();
    assert_eq!(
        copy_verified_file(&lock, &store, &wanted.tds_path, &mut stream).unwrap(),
        metadata
    );
    assert_eq!(stream, bytes);
    assert!(export_file(&lock, &store, "alpha.sty", &output).is_err());
    assert!(export_file(&lock, &store, "../alpha.sty", &output).is_err());
    assert!(export_file(&lock, &store, &wanted.tds_path, &store.join("clobber")).is_err());
    let digest = wanted.digest.strip_prefix("sha256:").unwrap();
    let object = store.join(&digest[..2]).join(digest);
    make_test_writable(&object);
    fs::write(&object, b"corrupt").unwrap();
    let mut stream = Vec::new();
    assert!(copy_verified_file(&lock, &store, &wanted.tds_path, &mut stream).is_err());
    assert!(stream.is_empty());
    assert!(export_file(&lock, &store, &wanted.tds_path, &output).is_err());
    assert_eq!(fs::read(&output).unwrap(), bytes);
    assert!(!fs::read_dir(&root).unwrap().any(|entry| {
        entry
            .unwrap()
            .file_name()
            .to_string_lossy()
            .contains(".pqty-file-")
    }));
    fs::remove_file(object).unwrap();
    assert!(export_file(&lock, &store, &wanted.tds_path, &output).is_err());
    make_test_tree_writable(&root);
    fs::remove_dir_all(root).unwrap();
}
struct FailingWriter;
impl Write for FailingWriter {
    fn write(&mut self, _: &[u8]) -> io::Result<usize> {
        Err(io::Error::other("expected writer failure"))
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}
#[test]
fn verified_copy_propagates_writer_failures() {
    let (root, _, lock) = convergence_fixture();
    let file = &lock.closure[0].files[0].tds_path;
    assert!(
        copy_verified_file(&lock, &root.join("store"), file, &mut FailingWriter)
            .unwrap_err()
            .to_string()
            .contains("expected writer failure")
    );
    make_test_tree_writable(&root);
    fs::remove_dir_all(root).unwrap();
}
