use super::{make_test_tree_writable, temporary_test_root};
use crate::{
    MemorySourceTree, PackageByteSource, PackageRegistry, TlpdbIndex, VirtualPath, hydrate_lock,
    require_runtime, resolve, scan_source, validate_lock,
};
use std::{fs, path::Path};

const METADATA: &str = include_str!("../../tests/fixtures/package-data/texlive.tlpdb");

fn source(bytes: &[u8]) -> MemorySourceTree {
    let mut tree = MemorySourceTree::default();
    tree.insert(VirtualPath::new("main.tex").unwrap(), bytes);
    tree
}

fn index() -> TlpdbIndex {
    TlpdbIndex::parse(METADATA, Path::new("tlpkg/texlive.tlpdb"))
}

#[test]
fn data_closures_follow_contents_across_categories_and_exclude_installers_and_bins() {
    let index = index();
    let mut lock = scan_source(
        &source(br"\usepackage{alpha}"),
        VirtualPath::new("main.tex").unwrap(),
    )
    .unwrap();
    resolve(&mut lock, &index);
    validate_lock(&lock).unwrap();
    assert_eq!(
        lock.closure
            .iter()
            .map(|entry| entry.provider.as_str())
            .collect::<Vec<_>>(),
        ["alpha", "context-data", "core-data", "script-data"]
    );
    for (file, provider) in [
        ("data.tcx", "core-data"),
        ("module.mkiv", "context-data"),
        ("module.lua", "script-data"),
    ] {
        assert_eq!(index.providers_of_file(file), [provider]);
    }
    assert!(
        index
            .providers_of_path("tlpkg/private/module.lua")
            .is_empty()
    );
    assert!(index.providers_of_file("example.tex").is_empty());
    let mixed = METADATA.replace(
        " RELOC/tex/latex/alpha/alpha.sty",
        " RELOC/tex/latex/alpha/alpha.sty\n tlpkg/private/module.lua",
    );
    let mixed = TlpdbIndex::parse(&mixed, Path::new("tlpkg/texlive.tlpdb"));
    assert_eq!(mixed.providers_of_file("module.lua"), ["script-data"]);
    require_runtime(
        &mut lock,
        &index,
        &[
            "scripts/demo/module.lua".into(),
            "tex/context/third/demo/module.mkiv".into(),
        ],
        &[],
    )
    .unwrap();
    validate_lock(&lock).unwrap();
}

#[test]
fn project_inputs_win_and_missing_or_dynamic_inputs_remain_unresolved() {
    let index = index();
    let mut tree = source(br"\input amstex \input{absent} \input{\computed} \include{support}");
    tree.insert(
        VirtualPath::new("amstex.tex").unwrap(),
        br"% project override",
    );
    let mut lock = scan_source(&tree, VirtualPath::new("main.tex").unwrap()).unwrap();
    resolve(&mut lock, &index);
    assert!(lock.closure.is_empty());
    assert!(
        lock.inputs
            .iter()
            .any(|input| input.resolved_path.as_deref() == Some("amstex.tex"))
    );
    assert_eq!(
        lock.unresolved
            .iter()
            .map(|record| record.name.as_str())
            .collect::<Vec<_>>(),
        ["absent", "\\computed", "support"]
    );
    validate_lock(&lock).unwrap();
}

#[test]
fn ambiguous_literal_inputs_fail_without_picking_an_owner_or_basename_path() {
    for file in ["tex/amstex/amstex.tex", "tex/other/amstex.tex"] {
        let metadata =
            format!("{METADATA}\nname other\ncategory Package\nrunfiles size=1\n RELOC/{file}\n");
        let index = TlpdbIndex::parse(&metadata, Path::new("tlpkg/texlive.tlpdb"));
        let mut lock = scan_source(
            &source(br"\input amstex"),
            VirtualPath::new("main.tex").unwrap(),
        )
        .unwrap();
        resolve(&mut lock, &index);
        assert!(crate::fail_on_unresolved_packages(&lock).is_err());
        assert!(lock.closure.is_empty());
    }
    let metadata = METADATA.replace(
        " RELOC/tex/amstex/amstex.tex",
        " RELOC/tex/amstex/amstex.tex\n RELOC/tex/other/amstex.tex",
    );
    let index = TlpdbIndex::parse(&metadata, Path::new("tlpkg/texlive.tlpdb"));
    let mut lock = scan_source(
        &source(br"\input amstex"),
        VirtualPath::new("main.tex").unwrap(),
    )
    .unwrap();
    resolve(&mut lock, &index);
    assert!(crate::fail_on_unresolved_packages(&lock).is_err());
    let mut exact = scan_source(
        &source(br"\input{tex/amstex/amstex.tex}"),
        VirtualPath::new("main.tex").unwrap(),
    )
    .unwrap();
    resolve(&mut exact, &index);
    assert!(exact.unresolved.is_empty());
    assert_eq!(exact.closure[0].runtime_requests, ["tex/amstex/amstex.tex"]);
}

#[test]
fn plain_input_hydrates_cross_provider_inputs_and_data_dependencies() {
    let root = temporary_test_root("package-data");
    let index = TlpdbIndex::parse(METADATA, &root.join("tlpkg/texlive.tlpdb"));
    for provider in [
        "alpha",
        "amstex",
        "support",
        "core-data",
        "context-data",
        "script-data",
    ] {
        for file in &index.package(provider).unwrap().runfiles {
            let path = root.join(crate::normalize_runfile(file).0);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            let bytes = match provider {
                "amstex" => br"\input support \RequirePackage{alpha}".as_slice(),
                _ => b"% package data\n".as_slice(),
            };
            fs::write(path, bytes).unwrap();
        }
    }
    let tree = source(br"\input amstex \bye");
    let mut lock = scan_source(&tree, VirtualPath::new("main.tex").unwrap()).unwrap();
    resolve(&mut lock, &index);
    assert!(lock.unresolved.is_empty());
    assert_eq!(lock.closure[0].provider, "amstex");
    assert_eq!(lock.closure[0].runtime_requests, ["tex/amstex/amstex.tex"]);
    hydrate_lock(
        &mut lock,
        &index,
        &PackageByteSource::local_texlive(&root),
        &root.join("store"),
    )
    .unwrap();
    validate_lock(&lock).unwrap();
    assert_eq!(
        lock.closure
            .iter()
            .map(|entry| entry.provider.as_str())
            .collect::<Vec<_>>(),
        [
            "alpha",
            "amstex",
            "context-data",
            "core-data",
            "script-data",
            "support"
        ]
    );
    make_test_tree_writable(&root);
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn source_scanning_hashes_legacy_bytes_without_reinterpreting_filenames() {
    let bytes = b"\\usepackage{alpha}\n% caf\xe9\ntext \xe9\n";
    let lock = scan_source(&source(bytes), VirtualPath::new("main.tex").unwrap()).unwrap();
    assert_eq!(lock.packages[0].name, "alpha");
    assert_eq!(lock.sources[0].digest, crate::digest_bytes(bytes));
    let mut tree = source(b"\\input{caf\xe9}\n");
    tree.insert(VirtualPath::new("caf\u{fffd}.tex").unwrap(), b"wrong file");
    assert!(
        scan_source(&tree, VirtualPath::new("main.tex").unwrap())
            .unwrap_err()
            .to_string()
            .contains("arguments must be UTF-8")
    );
}

#[test]
fn missing_literal_input_cannot_be_marked_hydrated_or_succeed_on_retry() {
    let root = temporary_test_root("missing-literal");
    let metadata = METADATA.replace(
        " RELOC/tex/amstex/amstex.tex",
        " RELOC/tex/amstex/amstex.tex\n RELOC/tex/amstex/extra.tex",
    );
    let index = TlpdbIndex::parse(&metadata, &root.join("tlpkg/texlive.tlpdb"));
    let directory = root.join("texmf-dist/tex/amstex");
    fs::create_dir_all(&directory).unwrap();
    fs::write(directory.join("extra.tex"), b"% other provider file").unwrap();
    let mut lock = scan_source(
        &source(br"\input amstex"),
        VirtualPath::new("main.tex").unwrap(),
    )
    .unwrap();
    resolve(&mut lock, &index);
    for _ in 0..2 {
        let error = hydrate_lock(
            &mut lock,
            &index,
            &PackageByteSource::local_texlive(&root),
            &root.join("store"),
        )
        .unwrap_err();
        assert!(error.to_string().contains("missing requested runtime file"));
        assert!(lock.closure[0].integrity.is_none());
    }
    fs::write(directory.join("amstex.tex"), b"% restored requested file").unwrap();
    hydrate_lock(
        &mut lock,
        &index,
        &PackageByteSource::local_texlive(&root),
        &root.join("store"),
    )
    .unwrap();
    validate_lock(&lock).unwrap();
    make_test_tree_writable(&root);
    fs::remove_dir_all(root).unwrap();
}
