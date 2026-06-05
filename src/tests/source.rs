#[test]
fn installs_the_golden_exact_lock_from_a_preseeded_store() {
    let fixture_root = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/golden/v1");
    let lock = read_lock(&fixture_root.join("lock-exact.json")).unwrap();
    let root = temporary_test_root("golden-install");
    let store = root.join("store");
    let out = root.join("texmf");
    let bytes = include_bytes!("../../tests/golden/v1/foo.sty");
    let file_digest = hex::encode(Sha256::digest(bytes));
    assert_eq!(
        file_digest,
        "69a2f5707973bb45fa187858c521bcb30d9ebd84cb2d511b4df66d8ef7ec89c4"
    );
    let object = store.join(&file_digest[..2]).join(&file_digest);
    fs::create_dir_all(object.parent().unwrap()).unwrap();
    fs::write(&object, bytes).unwrap();

    let package_digest = lock.closure[0]
        .store_key
        .as_deref()
        .unwrap()
        .strip_prefix("sha256:")
        .unwrap();
    let manifests = store.join("manifests");
    fs::create_dir_all(&manifests).unwrap();
    fs::write(
        manifests.join(format!("{package_digest}.json")),
        serde_json::to_vec(&serde_json::json!({
            "schema": "pqty.store-package/v1",
            "files": [{
                "tds_path": "tex/latex/foo/foo.sty",
                "digest": format!("sha256:{file_digest}")
            }]
        }))
        .unwrap(),
    )
    .unwrap();

    install_locked(&lock, &store, &out, LinkMode::Copy).unwrap();
    let installed = out.join("tex/latex/foo/foo.sty");
    assert_eq!(fs::read(&installed).unwrap(), bytes);
    assert!(fs::metadata(installed).unwrap().permissions().readonly());
    make_test_tree_writable(&root);
    let _ = fs::remove_dir_all(root);
}

#[test]
fn scans_an_editor_snapshot_without_a_filesystem() {
    let mut source = MemorySourceTree::default();
    source.insert(
        VirtualPath::new("main.tex").unwrap(),
        br"\documentclass{article}
\input{sections/intro}
",
    );
    source.insert(
        VirtualPath::new("sections/intro.tex").unwrap(),
        br"\usepackage{graphicx}",
    );

    let lock = scan_source(&source, VirtualPath::new("main.tex").unwrap()).unwrap();
    assert_eq!(
        lock.sources
            .iter()
            .map(|record| record.path.as_str())
            .collect::<Vec<_>>(),
        vec!["main.tex", "sections/intro.tex"]
    );
    assert_eq!(lock.packages.len(), 1);
    assert_eq!(lock.packages[0].name, "graphicx");
    assert_eq!(lock.packages[0].source.path, "sections/intro.tex");
    assert!(lock.unresolved.is_empty());
}

#[test]
fn nested_editor_entrypoint_can_include_within_project_root() {
    let mut source = MemorySourceTree::default();
    source.insert(
        VirtualPath::new("chapters/main.tex").unwrap(),
        br"\input{../shared/preamble}",
    );
    source.insert(
        VirtualPath::new("shared/preamble.tex").unwrap(),
        br"\usepackage{geometry}",
    );

    let lock = scan_source(&source, VirtualPath::new("chapters/main.tex").unwrap()).unwrap();
    assert_eq!(
        lock.inputs[0].resolved_path.as_deref(),
        Some("shared/preamble.tex")
    );
    assert_eq!(lock.packages[0].name, "geometry");
}

#[test]
fn explicit_filesystem_project_root_preserves_nested_lock_paths() {
    let root = temporary_test_root("nested-filesystem");
    let entry = root.join("paper/main.tex");
    let shared = root.join("shared/preamble.tex");
    fs::create_dir_all(entry.parent().unwrap()).unwrap();
    fs::create_dir_all(shared.parent().unwrap()).unwrap();
    fs::write(
        &entry,
        b"\\input{../shared/preamble}\n\\documentclass{article}",
    )
    .unwrap();
    fs::write(&shared, b"\\usepackage{amsmath}").unwrap();

    let lock = scan_project_at(&root, Path::new("paper/main.tex")).unwrap();
    fs::remove_dir_all(&root).unwrap();

    assert_eq!(lock.root, "paper/main.tex");
    assert_eq!(
        lock.sources
            .iter()
            .map(|source| source.path.as_str())
            .collect::<Vec<_>>(),
        vec!["paper/main.tex", "shared/preamble.tex"]
    );
    assert_eq!(
        lock.inputs[0].resolved_path.as_deref(),
        Some("shared/preamble.tex")
    );
}

#[test]
fn explicit_input_roots_resolve_recursive_project_owned_styles() {
    let root = temporary_test_root("input-roots");
    fs::create_dir_all(root.join("vendor/natbib/styles")).unwrap();
    fs::write(
        root.join("main.tex"),
        b"\\usepackage{natbib}\n\\bibliographystyle{natbib}\n",
    )
    .unwrap();
    fs::write(root.join("vendor/natbib/styles/natbib.bst"), b"ENTRY{}{}{}").unwrap();

    let lock = scan_project_at_with_roots(
        &root,
        Path::new("main.tex"),
        &[PathBuf::from("vendor/natbib")],
    )
    .unwrap();
    fs::remove_dir_all(&root).unwrap();

    assert_eq!(lock.packages[0].resolved_path, None);
    assert_eq!(
        lock.bibliographies[0].resolved_path.as_deref(),
        Some("vendor/natbib/styles/natbib.bst")
    );
    assert!(
        lock.sources
            .iter()
            .any(|source| source.path == "vendor/natbib/styles/natbib.bst")
    );
}

#[test]
fn leading_control_sequence_paths_resolve_confined_project_suffixes() {
    let mut source = MemorySourceTree::default();
    source.insert(
        VirtualPath::new("main.tex").unwrap(),
        br"\input{\projectroot/styles/local}
\bibliographystyle{\projectroot/bib/local}
",
    );
    source.insert(
        VirtualPath::new("styles/local.tex").unwrap(),
        br"\usepackage{xcolor}",
    );
    source.insert(VirtualPath::new("bib/local.bst").unwrap(), b"ENTRY{}{}{}");

    let lock = scan_source(&source, VirtualPath::new("main.tex").unwrap()).unwrap();

    assert_eq!(
        lock.inputs[0].resolved_path.as_deref(),
        Some("styles/local.tex")
    );
    assert_eq!(
        lock.bibliographies[0].resolved_path.as_deref(),
        Some("bib/local.bst")
    );
    assert_eq!(lock.packages[0].name, "xcolor");
    assert!(
        lock.unresolved
            .iter()
            .all(|item| !item.name.contains("projectroot"))
    );
}

#[test]
fn local_classes_and_styles_are_scanned_but_not_registry_resolved() {
    let mut source = MemorySourceTree::default();
    source.insert(
        VirtualPath::new("main.tex").unwrap(),
        br"\documentclass{commonarticle}
\usepackage{localstyle}
\bibliographystyle{localstyle}
",
    );
    source.insert(
        VirtualPath::new("commonarticle.cls").unwrap(),
        br"\LoadClass{article}
\RequirePackage{geometry}
",
    );
    source.insert(
        VirtualPath::new("localstyle.sty").unwrap(),
        br"\RequirePackage{xcolor}",
    );
    source.insert(VirtualPath::new("localstyle.bst").unwrap(), b"ENTRY{}{}{}");

    let mut lock = scan_source(&source, VirtualPath::new("main.tex").unwrap()).unwrap();
    assert_eq!(
        lock.document_class
            .as_ref()
            .and_then(|class| class.resolved_path.as_deref()),
        Some("commonarticle.cls")
    );
    assert_eq!(
        lock.packages
            .iter()
            .find(|package| package.name == "localstyle")
            .and_then(|package| package.resolved_path.as_deref()),
        Some("localstyle.sty")
    );
    assert_eq!(
        lock.bibliographies
            .iter()
            .find(|record| matches!(record.kind, BibliographyKind::BibliographyStyle))
            .and_then(|record| record.resolved_path.as_deref()),
        Some("localstyle.bst")
    );
    assert_eq!(
        lock.sources
            .iter()
            .map(|source| source.path.as_str())
            .collect::<Vec<_>>(),
        vec![
            "commonarticle.cls",
            "localstyle.bst",
            "localstyle.sty",
            "main.tex"
        ]
    );

    let sample = concat!(
        "name latex\n",
        "revision 1\n",
        "runfiles size=1\n",
        " texmf-dist/tex/latex/base/article.cls\n",
        "\n",
        "name geometry\n",
        "revision 1\n",
        "runfiles size=1\n",
        " texmf-dist/tex/latex/geometry/geometry.sty\n",
        "\n",
        "name xcolor\n",
        "revision 1\n",
        "runfiles size=1\n",
        " texmf-dist/tex/latex/xcolor/xcolor.sty\n",
    );
    let index = TlpdbIndex::parse(sample, Path::new("/usr/share/tlpkg/texlive.tlpdb"));
    resolve(&mut lock, &index);
    assert_eq!(
        lock.closure
            .iter()
            .map(|package| package.provider.as_str())
            .collect::<Vec<_>>(),
        vec!["geometry", "latex", "xcolor"]
    );
    assert!(lock.unresolved.is_empty());
}
use std::fs;
use std::path::{Path, PathBuf};

use sha2::{Digest as _, Sha256};

use crate::tests::{make_test_tree_writable, temporary_test_root};
use crate::{
    BibliographyKind, LinkMode, MemorySourceTree, TlpdbIndex, VirtualPath, install_locked,
    read_lock, resolve, scan_project_at, scan_project_at_with_roots, scan_source,
};
