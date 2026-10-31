#[test]
fn raw_bundle_extensions_are_rejected_before_reading_their_declared_bodies() {
    use std::io::Write as _;
    for kind in [tar::EntryType::XHeader, tar::EntryType::GNULongName] {
        let root = temporary_test_root("bundle-extensions");
        fs::create_dir_all(&root).unwrap();
        let source = root.join("main.tex");
        let pdf = root.join("main.pdf");
        fs::write(&source, b"research source").unwrap();
        fs::write(&pdf, b"last PDF").unwrap();
        let mut header = tar::Header::new_gnu();
        header.set_path(".path-metadata").unwrap();
        header.set_entry_type(kind);
        header.set_size(1024 * 1024 * 1024);
        header.set_cksum();
        let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
        encoder.write_all(header.as_bytes()).unwrap();
        let path = root.join("oversized.tar.gz");
        fs::write(&path, encoder.finish().unwrap()).unwrap();
        let store = root.join("store");
        let error = crate::import_bundle(&path, &store).unwrap_err();
        assert!(
            error
                .to_string()
                .contains("Invalid package bundle metadata entry"),
            "{error}"
        );
        assert_eq!(fs::read(&source).unwrap(), b"research source");
        assert_eq!(fs::read(&pdf).unwrap(), b"last PDF");
        assert_eq!(fs::read_dir(&store).unwrap().count(), 0);
        fs::remove_dir_all(root).unwrap();
    }
}

#[test]
#[allow(clippy::too_many_lines)]
fn materialized_tree_replacement_is_transactional() {
    let root = temporary_test_root("tree");
    let store = root.join("store");
    let out = root.join("texmf");

    let bytes = b"locked package bytes";
    let digest = hex::encode(Sha256::digest(bytes));
    let store_object = store.join(&digest[..2]).join(&digest);
    fs::create_dir_all(store_object.parent().unwrap()).unwrap();
    fs::write(&store_object, bytes).unwrap();
    let files = vec![LockedFile {
        tds_path: "tex/latex/foo/foo.sty".to_string(),
        kind: ResourceKind::Tex,
    }];
    let package_digest = hex::encode(Sha256::digest(
        format!("tex/latex/foo/foo.sty:{digest}\n").as_bytes(),
    ));
    let integrity = format!(
        "sha256-{}",
        BASE64.encode(hex::decode(&package_digest).unwrap())
    );
    let manifests = store.join("manifests");
    fs::create_dir_all(&manifests).unwrap();
    fs::write(
        manifests.join(format!("{package_digest}.json")),
        serde_json::to_vec(&serde_json::json!({
            "schema": "pqty.store-package/v1",
            "files": [{
                "tds_path": "tex/latex/foo/foo.sty",
                "digest": format!("sha256:{digest}")
            }]
        }))
        .unwrap(),
    )
    .unwrap();
    let lock = LockFile {
        schema: LOCK_SCHEMA.to_string(),
        stage: LockStage::Exact,
        generated_with: "test".to_string(),
        environment: Environment::default(),
        registries: vec![Registry {
            id: "test".to_string(),
            kind: RegistryKind::Tlnet,
            url: "https://example.invalid/tlnet".to_string(),
            snapshot: Some("test".to_string()),
            metadata_digest: Some(format!("sha256:{}", "11".repeat(32))),
        }],
        consumer_requirements: ConsumerRequirements::default(),
        root: "main.tex".to_string(),
        sources: vec![SourceRecord {
            path: "main.tex".to_string(),
            digest: format!("sha256:{}", "22".repeat(32)),
        }],
        document_class: None,
        loaded_classes: Vec::new(),
        packages: Vec::new(),
        inputs: Vec::new(),
        bibliographies: Vec::new(),
        graphics: Vec::new(),
        closure: vec![ResolvedPackage {
            provider: "foo".to_string(),
            version: "tlrev:1".to_string(),
            source: PackageSource {
                registry: Some("test".to_string()),
                kind: PackageSourceKind::Registry,
                locator: "foo".to_string(),
                container_checksum: None,
                container_size: None,
            },
            satisfies: Vec::new(),
            integrity: Some(integrity),
            dependencies: Vec::new(),
            direct: true,
            requested_by: Vec::new(),
            store_key: Some(format!("sha256:{package_digest}")),
            engines: Vec::new(),
            font_maps: Vec::new(),
            runtime_requests: Vec::new(),
            files,
        }],
        unresolved: Vec::new(),
    };

    assert_materialization_destination_safety(&root, &store, &lock);

    materialize_from_store(&lock, &store, &out, LinkMode::Copy).unwrap();
    assert_environment_reuse_and_recovery(&root, &store, &out, &lock, bytes);
    fs::write(out.join("old-marker"), b"old").unwrap();
    materialize_from_store(&lock, &store, &out, LinkMode::Copy).unwrap();
    let installed = out.join("tex/latex/foo/foo.sty");
    assert_eq!(fs::read(&installed).unwrap(), bytes);
    assert!(!out.join("old-marker").exists());

    let bundle = root.join("portable.tar.gz");
    crate::export_bundle(&lock, &store, &bundle).unwrap();
    let imported_store = root.join("imported-store");
    let imported = crate::import_bundle(&bundle, &imported_store).unwrap();
    let imported_tree = root.join("imported-tree");
    materialize_from_store(&imported, &imported_store, &imported_tree, LinkMode::Copy).unwrap();
    assert_eq!(
        fs::read(imported_tree.join("tex/latex/foo/foo.sty")).unwrap(),
        bytes
    );
    let archive_bytes = fs::read(&bundle).unwrap();
    for suffix in [b"trailing garbage".as_slice(), archive_bytes.as_slice()] {
        let appended = root.join("appended.tar.gz");
        let mut appended_bytes = archive_bytes.clone();
        appended_bytes.extend_from_slice(suffix);
        fs::write(&appended, appended_bytes).unwrap();
        assert!(
            crate::import_bundle(&appended, &imported_store).is_err(),
            "compressed bytes after the gzip member must not publish package manifests"
        );
        assert_eq!(
            fs::read(imported_tree.join("tex/latex/foo/foo.sty")).unwrap(),
            bytes
        );
    }
    let truncated = root.join("truncated.tar.gz");
    fs::write(&truncated, &archive_bytes[..archive_bytes.len() - 8]).unwrap();
    assert!(crate::import_bundle(&truncated, &imported_store).is_err());
    assert_eq!(
        fs::read(imported_tree.join("tex/latex/foo/foo.sty")).unwrap(),
        bytes
    );
    fs::write(&store_object, b"corrupt").unwrap();
    assert!(crate::export_bundle(&lock, &store, &bundle).is_err());
    assert_eq!(fs::read(&bundle).unwrap(), archive_bytes);
    // A valid copied tree is independent of a damaged shared cache.
    materialize_from_store(&lock, &store, &out, LinkMode::Copy).unwrap();
    fs::write(out.join("force-replacement"), b"extra file").unwrap();
    assert!(materialize_from_store(&lock, &store, &out, LinkMode::Copy).is_err());
    assert_eq!(
        fs::read(&installed).unwrap(),
        bytes,
        "the previously published tree must survive a failed replacement"
    );
    assert!(fs::metadata(&installed).unwrap().permissions().readonly());
    make_test_tree_writable(&root);
    let _ = fs::remove_dir_all(root);
}

fn assert_environment_reuse_and_recovery(
    root: &Path,
    store: &Path,
    out: &Path,
    lock: &LockFile,
    bytes: &[u8],
) {
    let installed = out.join("tex/latex/foo/foo.sty");
    let before = fs::metadata(&installed).unwrap().modified().unwrap();
    // texe owns this generated database. A no-op installation must retain it.
    fs::write(out.join("ls-R"), b"existing filename database").unwrap();
    materialize_from_store(lock, store, out, LinkMode::Copy).unwrap();
    assert_eq!(
        fs::read(out.join("ls-R")).unwrap(),
        b"existing filename database"
    );
    assert_eq!(
        fs::metadata(&installed).unwrap().modified().unwrap(),
        before
    );

    // Fingerprints alone cannot prove that an installed file is still valid.
    make_test_writable(&installed);
    fs::write(&installed, vec![b'x'; bytes.len()]).unwrap();
    materialize_from_store(lock, store, out, LinkMode::Copy).unwrap();
    assert_eq!(fs::read(&installed).unwrap(), bytes);
    make_test_writable(&installed);
    fs::remove_file(&installed).unwrap();
    materialize_from_store(lock, store, out, LinkMode::Copy).unwrap();
    assert_eq!(fs::read(&installed).unwrap(), bytes);

    // Simulate a process dying after moving the old destination, with an
    // incomplete new generation still private in the transaction directory.
    let control = root.join(".texmf.pqty-install");
    fs::rename(out, control.join("previous")).unwrap();
    fs::create_dir(control.join("stage")).unwrap();
    fs::write(control.join("stage/partial"), b"incomplete").unwrap();
    materialize_from_store(lock, store, out, LinkMode::Copy).unwrap();
    assert_eq!(fs::read(&installed).unwrap(), bytes);
    assert!(!control.join("previous").exists());
    assert!(!control.join("stage").exists());

    // A committed destination wins over a backup left by interrupted cleanup.
    fs::create_dir(control.join("previous")).unwrap();
    fs::write(control.join("previous/old"), b"previous generation").unwrap();
    materialize_from_store(lock, store, out, LinkMode::Copy).unwrap();
    assert!(!control.join("previous").exists());
    assert_eq!(fs::read(&installed).unwrap(), bytes);
}

fn assert_materialization_destination_safety(root: &Path, store: &Path, lock: &LockFile) {
    let unowned = root.join("paper");
    fs::create_dir_all(&unowned).unwrap();
    fs::write(unowned.join("main.tex"), b"source").unwrap();
    let error =
        materialize_from_store(lock, store, &unowned, LinkMode::Copy).expect_err("unowned output");
    assert!(error.to_string().contains("not owned by pqty"));
    assert_eq!(fs::read(unowned.join("main.tex")).unwrap(), b"source");

    let overlapping = store.join("texmf");
    let error = materialize_from_store(lock, store, &overlapping, LinkMode::Copy)
        .expect_err("overlapping output");
    assert!(error.to_string().contains("must not overlap"));

    let empty = root.join("empty-texmf");
    fs::create_dir(&empty).unwrap();
    materialize_from_store(lock, store, &empty, LinkMode::Copy).unwrap();
    assert!(empty.join(".pqty-materialized.json").is_file());
}

#[test]
fn install_uses_the_locked_files_and_then_the_content_store() {
    let root = temporary_test_root("locked-install");
    let registry_file = root.join("texmf-dist/tex/latex/foo/foo.sty");
    fs::create_dir_all(registry_file.parent().unwrap()).unwrap();
    fs::write(&registry_file, br"\ProvidesPackage{foo}").unwrap();

    let sample = concat!(
        "name foo\n",
        "category Package\n",
        "revision 7\n",
        "runfiles size=1\n",
        " texmf-dist/tex/latex/foo/foo.sty\n",
    );
    let index = TlpdbIndex::parse(sample, &root.join("tlpkg/texlive.tlpdb"));
    let mut source = MemorySourceTree::default();
    source.insert(VirtualPath::new("main.tex").unwrap(), br"\usepackage{foo}");
    let mut lock = scan_source(&source, VirtualPath::new("main.tex").unwrap()).unwrap();
    resolve(&mut lock, &index);
    hydrate_lock(
        &mut lock,
        &index,
        &PackageByteSource::local_texlive(&root),
        &root.join("hydrate-store"),
    )
    .unwrap();

    let install_store = root.join("install-store");
    let first_out = root.join("first-texmf");
    let first = install_locked(&lock, &install_store, &first_out, LinkMode::Copy).unwrap();
    assert_eq!(first.providers(), 1);
    assert_eq!(first.files(), 1);
    assert!(first.bytes_stored() > 0);
    assert_eq!(
        fs::read(first_out.join("tex/latex/foo/foo.sty")).unwrap(),
        br"\ProvidesPackage{foo}"
    );
    let object_digest = hex::encode(Sha256::digest(br"\ProvidesPackage{foo}"));
    let object_path = install_store.join(&object_digest[..2]).join(&object_digest);
    assert!(fs::metadata(&object_path).unwrap().permissions().readonly());

    make_test_writable(&object_path);
    fs::write(&object_path, b"corrupt").unwrap();
    let recovered_out = root.join("recovered-texmf");
    install_locked(&lock, &install_store, &recovered_out, LinkMode::Copy).unwrap();
    assert_eq!(
        fs::read(recovered_out.join("tex/latex/foo/foo.sty")).unwrap(),
        br"\ProvidesPackage{foo}"
    );
    assert!(install_store.join("quarantine").is_dir());

    let package_digest = lock.closure[0]
        .store_key
        .as_deref()
        .unwrap()
        .strip_prefix("sha256:")
        .unwrap();
    let manifest_path = install_store
        .join("manifests")
        .join(format!("{package_digest}.json"));
    make_test_writable(&manifest_path);
    fs::write(&manifest_path, b"{not-json").unwrap();
    let manifest_recovered_out = root.join("manifest-recovered-texmf");
    install_locked(
        &lock,
        &install_store,
        &manifest_recovered_out,
        LinkMode::Copy,
    )
    .unwrap();
    assert_eq!(
        fs::read(manifest_recovered_out.join("tex/latex/foo/foo.sty")).unwrap(),
        br"\ProvidesPackage{foo}"
    );

    fs::remove_dir_all(root.join("texmf-dist")).unwrap();
    let second_out = root.join("second-texmf");
    let second = install_locked(&lock, &install_store, &second_out, LinkMode::Copy).unwrap();
    assert_eq!(second.bytes_stored(), 0);
    assert_eq!(
        fs::read(second_out.join("tex/latex/foo/foo.sty")).unwrap(),
        br"\ProvidesPackage{foo}"
    );

    make_test_writable(&object_path);
    fs::write(&object_path, b"corrupt-again").unwrap();
    let offline_out = root.join("offline-texmf");
    let error = install_locked_with_policy(
        &lock,
        &install_store,
        &offline_out,
        LinkMode::Copy,
        true,
        false,
    )
    .unwrap_err();
    assert!(
        error
            .to_string()
            .contains("preserved corrupt store evidence")
    );
    assert!(!offline_out.exists());

    assert_copy_reuse_without_cache(&install_store, &first_out, &lock);

    make_test_tree_writable(&root);
    fs::remove_dir_all(root).unwrap();
}

fn assert_copy_reuse_without_cache(install_store: &Path, first_out: &Path, lock: &LockFile) {
    // Reusing an already verified copy needs neither the removed registry nor
    // the quarantined, and subsequently completely cleared, package cache.
    install_locked_with_policy(lock, install_store, first_out, LinkMode::Copy, true, false)
        .unwrap();
    make_test_tree_writable(install_store);
    fs::remove_dir_all(install_store).unwrap();
    let reused =
        install_locked_with_policy(lock, install_store, first_out, LinkMode::Copy, true, false)
            .unwrap();
    assert_eq!(reused.bytes_stored(), 0);
    assert!(!install_store.join("manifests").exists());
    let installed = first_out.join("tex/latex/foo/foo.sty");
    make_test_writable(&installed);
    fs::write(&installed, vec![b'x'; br"\ProvidesPackage{foo}".len()]).unwrap();
    assert!(
        install_locked_with_policy(lock, install_store, first_out, LinkMode::Copy, true, false)
            .is_err(),
        "an ownership marker cannot hide corrupt installed content"
    );
}

#[test]
fn rejects_parent_paths() {
    assert!(clean_relative_name("../secret").is_none());
    assert!(clean_relative_name("/tmp/main.tex").is_none());
    assert_eq!(
        clean_relative_name("./main.tex").unwrap(),
        PathBuf::from("main.tex")
    );
}
use std::fs;
use std::path::{Path, PathBuf};

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64;
use sha2::{Digest as _, Sha256};

use crate::tests::{make_test_tree_writable, make_test_writable, temporary_test_root};
use crate::{
    ConsumerRequirements, Environment, LOCK_SCHEMA, LinkMode, LockFile, LockStage, LockedFile,
    MemorySourceTree, PackageByteSource, PackageSource, PackageSourceKind, Registry, RegistryKind,
    ResolvedPackage, ResourceKind, SourceRecord, TlpdbIndex, VirtualPath, clean_relative_name,
    hydrate_lock, install_locked, install_locked_with_policy, materialize_from_store, resolve,
    scan_source,
};
