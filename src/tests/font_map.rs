use super::{make_test_tree_writable, temporary_test_root};
use crate::{
    FontMapKind, FontMapManifest, MemorySourceTree, PackageByteSource, PackageRegistry, TlpdbIndex,
    VirtualPath, hydrate_lock, resolve, scan_source,
};
use std::{fs, path::Path};
const METADATA: &str = include_str!("../../tests/fixtures/font-maps/texlive.tlpdb");
fn fixture() -> (std::path::PathBuf, TlpdbIndex, crate::LockFile) {
    let root = temporary_test_root("font-maps");
    fs::create_dir_all(root.join("tlpkg")).unwrap();
    fs::write(root.join("tlpkg/texlive.tlpdb"), METADATA).unwrap();
    let index = TlpdbIndex::load(&root.join("tlpkg/texlive.tlpdb")).unwrap();
    for provider in ["demo", "concrete"] {
        for file in &index.package(provider).unwrap().runfiles {
            let path = root.join(crate::normalize_runfile(file).0);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, b"% package data\n").unwrap();
        }
    }
    let mut source = MemorySourceTree::default();
    source.insert(VirtualPath::new("main.tex").unwrap(), br"\usepackage{demo}");
    let mut lock = scan_source(&source, VirtualPath::new("main.tex").unwrap()).unwrap();
    resolve(&mut lock, &index);
    hydrate_lock(
        &mut lock,
        &index,
        &PackageByteSource::local_texlive(&root),
        &root.join("store"),
    )
    .unwrap();
    (root, index, lock)
}
#[test]
fn typed_maps_preserve_templates_types_and_cross_provider_files_without_changing_v1() {
    let (root, index, lock) = fixture();
    let old = crate::ResolvedEnvironment::from_lock(&lock).unwrap();
    assert_eq!(old.font_maps, ["bitmap.map", "demo.map"]);
    let maps = index.font_map_manifest(&lock).unwrap();
    maps.validate(&lock, &index).unwrap();
    assert_eq!(maps.environment_fingerprint, old.fingerprint);
    assert_eq!(maps.maps.len(), 4);
    assert_eq!(maps.maps[0].declaration.kind, FontMapKind::Map);
    assert_eq!(maps.maps[1].declaration.kind, FontMapKind::MixedMap);
    let template = maps
        .maps
        .iter()
        .find(|map| !map.declaration.parameters.is_empty())
        .unwrap();
    assert_eq!(template.declaration.parameters, ["jaEmbed", "jaVariant"]);
    assert_eq!(template.declaration.name, "ptex-@jaEmbed@@jaVariant@.map");
    assert!(template.tds_path.is_none());
    let concrete = maps
        .maps
        .iter()
        .find(|map| map.declaration.name == "morisawa5.map")
        .unwrap();
    assert_eq!(concrete.declaration.kind, FontMapKind::KanjiMap);
    assert_eq!(
        concrete.tds_path.as_deref(),
        Some("fonts/map/dvipdfmx/demo/morisawa5.map")
    );
    let schema: serde_json::Value =
        serde_json::from_str(include_str!("../../schemas/pqty.font-maps.schema.json")).unwrap();
    jsonschema::draft202012::meta::validate(&schema).unwrap();
    jsonschema::validator_for(&schema)
        .unwrap()
        .validate(&serde_json::to_value(&maps).unwrap())
        .unwrap();
    let mut changed = maps.clone();
    changed.maps.pop();
    assert!(changed.validate(&lock, &index).is_err());
    let mut changed = serde_json::to_value(&maps).unwrap();
    changed["maps"][0]["declaration"]["extra"] = true.into();
    assert!(serde_json::from_value::<FontMapManifest>(changed).is_err());
    let changed = TlpdbIndex::try_parse(
        &METADATA.replace("revision 1", "revision 3"),
        &root.join("tlpkg/texlive.tlpdb"),
    )
    .unwrap();
    assert!(maps.validate(&lock, &changed).is_err());
    make_test_tree_writable(&root);
    fs::remove_dir_all(root).unwrap();
}
#[test]
fn malformed_map_actions_fail_instead_of_dropping_metadata() {
    for action in [
        "addMap",
        "addKanjiMap ../escape.map",
        "addMixedMap no.map extra",
        "addKanjiMap ptex-@broken.map",
        "addKanjiMap ptex-@@.map",
    ] {
        let metadata = format!("name demo\ncategory Package\nexecute {action}\n");
        assert!(
            TlpdbIndex::try_parse(&metadata, Path::new("tlpkg/texlive.tlpdb")).is_err(),
            "{action}"
        );
    }
}
