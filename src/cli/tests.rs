use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64;

use crate::cli::args::RemoteSelector;
use crate::cli::command::{refresh_existing_lock, registry_selection_matches};
use crate::{
    ConsumerRequirements, LockFile, LockStage, LockedFile, MemorySourceTree, MetadataCachePolicy,
    PackageSource, PackageSourceKind, Registry, RegistryKind, RegistryRequest, ResolvedPackage,
    ResourceKind, VirtualPath, scan_source, write_lock,
};
use std::fs;

fn scanned(source: &[u8]) -> LockFile {
    let mut tree = MemorySourceTree::default();
    tree.insert(VirtualPath::new("main.tex").expect("path"), source);
    scan_source(&tree, VirtualPath::new("main.tex").expect("path")).expect("scan")
}

fn hydrated(mut lock: LockFile) -> LockFile {
    lock.stage = LockStage::Exact;
    lock.registries.push(Registry {
        id: "tlnet".to_string(),
        kind: RegistryKind::Tlnet,
        url: "https://example.invalid/tlnet".to_string(),
        snapshot: Some("test".to_string()),
        metadata_digest: Some(format!("sha256:{}", "11".repeat(32))),
    });
    lock.closure.push(ResolvedPackage {
        provider: "foo".to_string(),
        version: "tlrev:1".to_string(),
        source: PackageSource {
            registry: Some("tlnet".to_string()),
            kind: PackageSourceKind::Registry,
            locator: "foo".to_string(),
            container_checksum: None,
            container_size: None,
        },
        satisfies: vec!["foo".to_string()],
        integrity: Some(format!("sha256-{}", BASE64.encode([7_u8; 32]))),
        dependencies: Vec::new(),
        direct: true,
        requested_by: Vec::new(),
        store_key: Some(format!("sha256:{}", "07".repeat(32))),
        engines: Vec::new(),
        font_maps: Vec::new(),
        runtime_requests: Vec::new(),
        files: vec![LockedFile {
            tds_path: "tex/latex/foo/foo.sty".to_string(),
            kind: ResourceKind::Tex,
        }],
    });
    lock
}

#[test]
fn source_only_changes_refresh_without_resolving_the_closure() {
    let root = std::env::temp_dir().join(format!(
        "pqty-refresh-test-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock")
            .as_nanos()
    ));
    fs::create_dir_all(&root).expect("temporary directory");
    let output = root.join("pqty.lock");
    let first = hydrated(scanned(br"\usepackage{foo} first"));
    write_lock(&output, &first).expect("lock");

    let second = scanned(br"\usepackage{foo} second");
    let registry = RegistryRequest {
        url: "https://example.invalid/tlnet/tlpkg/texlive.tlpdb.xz".to_string(),
        snapshot: Some("test".to_string()),
        cache_policy: MetadataCachePolicy::Immutable,
        allow_insecure: false,
    };
    let refreshed = refresh_existing_lock(
        &second,
        &output,
        None,
        Some(&registry),
        Some(&"11".repeat(32)),
        &ConsumerRequirements::default(),
    )
    .expect("same requirements reuse the closure");
    assert_ne!(first.sources[0].digest, refreshed.sources[0].digest);
    assert_eq!(first.closure[0].integrity, refreshed.closure[0].integrity);

    let changed = scanned(br"\usepackage{foo,bar}");
    assert!(
        refresh_existing_lock(
            &changed,
            &output,
            None,
            Some(&registry),
            Some(&"11".repeat(32)),
            &ConsumerRequirements::default(),
        )
        .is_none()
    );
    let _ = fs::remove_dir_all(root);
}

#[test]
fn consumer_requirement_changes_disable_exact_lock_reuse() {
    let root = std::env::temp_dir().join(format!(
        "pqty-requirements-test-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock")
            .as_nanos()
    ));
    fs::create_dir_all(&root).expect("temporary directory");
    let output = root.join("pqty.lock");
    let mut existing = hydrated(scanned(br"\usepackage{foo}"));
    existing.consumer_requirements.providers = vec!["foo".to_string()];
    write_lock(&output, &existing).expect("lock");
    let scanned = scanned(br"\usepackage{foo}");
    let registry = RegistryRequest {
        url: "https://example.invalid/tlnet/tlpkg/texlive.tlpdb.xz".to_string(),
        snapshot: Some("test".to_string()),
        cache_policy: MetadataCachePolicy::Immutable,
        allow_insecure: false,
    };
    assert!(
        refresh_existing_lock(
            &scanned,
            &output,
            None,
            Some(&registry),
            Some(&"11".repeat(32)),
            &ConsumerRequirements::default(),
        )
        .is_none(),
        "removing a Consumer requirement must force fresh resolution"
    );
    let _ = fs::remove_dir_all(root);
}

#[test]
fn remote_selector_accepts_only_latest_or_real_iso_dates() {
    assert_eq!(
        "latest".parse::<RemoteSelector>().unwrap(),
        RemoteSelector::Latest
    );
    assert_eq!(
        "2024-02-29".parse::<RemoteSelector>().unwrap(),
        RemoteSelector::Dated("2024-02-29".to_string())
    );
    for invalid in [
        "current",
        "2023-02-29",
        "2024-00-01",
        "2024-04-31",
        "2024-1-01",
    ] {
        assert!(invalid.parse::<RemoteSelector>().is_err(), "{invalid}");
    }
}

#[test]
fn dated_remote_selector_builds_immutable_archive_request() {
    let request = RemoteSelector::Dated("2026-07-26".to_string())
        .registry_request(false, false)
        .unwrap();
    assert_eq!(
        request.url,
        "https://texlive.info/tlnet-archive/2026/07/26/tlnet/tlpkg/texlive.tlpdb.xz"
    );
    assert_eq!(request.snapshot.as_deref(), Some("2026-07-26"));
    assert_eq!(request.cache_policy, MetadataCachePolicy::Immutable);
}

#[test]
fn offline_rejects_latest_but_accepts_dated_snapshots() {
    assert!(
        RemoteSelector::Latest
            .registry_request(true, false)
            .unwrap_err()
            .to_string()
            .contains("cannot be combined")
    );
    let request = RemoteSelector::Dated("2026-07-26".to_string())
        .registry_request(true, false)
        .unwrap();
    assert_eq!(request.cache_policy, MetadataCachePolicy::Offline);
}

#[test]
fn exact_lock_reuse_requires_the_same_dated_snapshot() {
    let lock = hydrated(scanned(br"\usepackage{foo}"));
    let same = RegistryRequest {
        url: "https://example.invalid/tlnet/tlpkg/texlive.tlpdb.xz".to_string(),
        snapshot: Some("test".to_string()),
        cache_policy: MetadataCachePolicy::Offline,
        allow_insecure: false,
    };
    assert!(registry_selection_matches(&lock, Some(&same), None));

    let different = RegistryRequest {
        snapshot: Some("other".to_string()),
        ..same
    };
    assert!(!registry_selection_matches(&lock, Some(&different), None));
}
