use super::{make_test_tree_writable, make_test_writable, temporary_test_root};
use crate::{
    FileManifest, LANGUAGES_SCHEMA, LanguageManifest, MemorySourceTree, PackageByteSource,
    PackageRegistry, TlpdbIndex, VirtualPath, export_file_manifest, hydrate_lock,
    require_languages, resolve, scan_source,
};
use std::{fs, path::Path};

const METADATA: &str = include_str!("../../tests/fixtures/languages/texlive.tlpdb");

#[test]
fn new_artifact_goldens_remain_bound_to_registry_and_lock() {
    let mut index = TlpdbIndex::try_parse(METADATA, Path::new("tlpkg/texlive.tlpdb")).unwrap();
    index.origin = Some("https://example.invalid/tlnet".into());
    let manifest: LanguageManifest = serde_json::from_str(include_str!(
        "../../tests/golden/lazy-inputs/languages.json"
    ))
    .unwrap();
    manifest.validate(&index).unwrap();
    let lock: crate::LockFile =
        serde_json::from_str(include_str!("../../tests/golden/v1/lock-exact.json")).unwrap();
    let manifest: FileManifest =
        serde_json::from_str(include_str!("../../tests/golden/lazy-inputs/files.json")).unwrap();
    manifest.validate(&lock).unwrap();
    assert_eq!(
        manifest.files[0].size,
        include_bytes!("../../tests/golden/v1/foo.sty").len() as u64
    );
    let mut json = serde_json::to_value(&manifest).unwrap();
    json["files"][0]["extra"] = true.into();
    assert!(serde_json::from_value::<FileManifest>(json).is_err());
    let manifest = index.resolve_languages(&["demo".into()]).unwrap();
    let mut json = serde_json::to_value(&manifest).unwrap();
    json["languages"][0]["declaration"]["extra"] = true.into();
    assert!(serde_json::from_value::<LanguageManifest>(json).is_err());
}

fn scanned() -> crate::LockFile {
    let mut tree = MemorySourceTree::default();
    tree.insert(VirtualPath::new("main.tex").unwrap(), br"\relax");
    scan_source(&tree, VirtualPath::new("main.tex").unwrap()).unwrap()
}

#[test]
fn comma_separated_patterns_and_exceptions_are_all_required() {
    let metadata = METADATA
        .replace("file_patterns=hyph-demo.pat.txt", "file_patterns=hyph-demo.pat.txt,hyph-second.pat.txt")
        .replace("file_exceptions=hyph-demo.hyp.txt", "file_exceptions=hyph-demo.hyp.txt,hyph-second.hyp.txt")
        .replace(" texmf-dist/tex/generic/hyph-demo/hyph-demo.hyp.txt", " texmf-dist/tex/generic/hyph-demo/hyph-demo.hyp.txt\n texmf-dist/tex/generic/hyph-demo/hyph-second.pat.txt\n texmf-dist/tex/generic/hyph-demo/hyph-second.hyp.txt");
    let index = TlpdbIndex::try_parse(&metadata, Path::new("tlpkg/texlive.tlpdb")).unwrap();
    let mut lock = scanned();
    let manifest = require_languages(&mut lock, &index, &["demo".into()]).unwrap();
    let declaration = &manifest.languages[0].declaration;
    assert_eq!(
        declaration.patterns,
        [
            "tex/generic/hyph-demo/hyph-demo.pat.txt",
            "tex/generic/hyph-demo/hyph-second.pat.txt"
        ]
    );
    assert_eq!(
        declaration.exceptions,
        [
            "tex/generic/hyph-demo/hyph-demo.hyp.txt",
            "tex/generic/hyph-demo/hyph-second.hyp.txt"
        ]
    );
    assert_eq!(lock.consumer_requirements.files.len(), 5);
    assert_eq!(lock.consumer_requirements.files, manifest.runtime_files());
    for value in [
        "hyph-demo.pat.txt,",
        ",hyph-demo.pat.txt",
        "hyph-demo.pat.txt,hyph-demo.pat.txt",
    ] {
        let malformed = METADATA.replace(
            "file_patterns=hyph-demo.pat.txt",
            &format!("file_patterns={value}"),
        );
        assert!(TlpdbIndex::try_parse(&malformed, Path::new("tlpkg/texlive.tlpdb")).is_err());
    }
    let literal = METADATA.replace(
        "disabled:example engine restriction",
        r"disabled:retain\tinside quotes",
    );
    let index = TlpdbIndex::try_parse(&literal, Path::new("tlpkg/texlive.tlpdb")).unwrap();
    assert_eq!(
        index.resolve_languages(&["demo".into()]).unwrap().languages[0]
            .declaration
            .attributes[0]
            .value,
        r"disabled:retain\tinside quotes"
    );
}

#[test]
fn language_aliases_pin_declared_data_and_transitive_providers() {
    let index = TlpdbIndex::try_parse(METADATA, Path::new("tlpkg/texlive.tlpdb")).unwrap();
    let mut lock = scanned();
    resolve(&mut lock, &index);
    let manifest = require_languages(
        &mut lock,
        &index,
        &["demo-alias".into(), "Demo".into(), "demo".into()],
    )
    .unwrap();
    assert_eq!(manifest.schema, LANGUAGES_SCHEMA);
    assert_eq!(manifest.registry, index.registry());
    assert_eq!(manifest.languages.len(), 1);
    let declaration = &manifest.languages[0].declaration;
    assert_eq!(declaration.name, "demo");
    assert_eq!(declaration.synonyms, ["Demo", "demo-alias"]);
    assert_eq!(
        (declaration.left_hyphen_min, declaration.right_hyphen_min),
        (2, 3)
    );
    assert_eq!(
        declaration.attributes[0].value,
        "disabled:example engine restriction"
    );
    assert_eq!(manifest.runtime_files(), lock.consumer_requirements.files);
    assert_eq!(lock.consumer_requirements.providers, ["hyphen-demo"]);
    assert_eq!(
        lock.closure
            .iter()
            .map(|p| p.provider.as_str())
            .collect::<Vec<_>>(),
        ["hyphen-data", "hyphen-demo", "hyphen-support"]
    );
    assert_eq!(lock.schema, crate::LOCK_SCHEMA);
    manifest.validate(&index).unwrap();
    let round_trip: LanguageManifest =
        serde_json::from_value(serde_json::to_value(&manifest).unwrap()).unwrap();
    round_trip.validate(&index).unwrap();
    let mut changed = manifest;
    changed.languages[0].declaration.left_hyphen_min = 1;
    assert!(changed.validate(&index).is_err());
}

#[test]
fn language_requests_fail_transactionally_on_missing_ambiguous_or_changed_inputs() {
    let index = TlpdbIndex::try_parse(METADATA, Path::new("tlpkg/texlive.tlpdb")).unwrap();
    let mut lock = scanned();
    resolve(&mut lock, &index);
    let before = serde_json::to_value(&lock).unwrap();
    for name in ["absent", "DEMO", "../demo", ""] {
        assert!(require_languages(&mut lock, &index, &["demo".into(), name.into()]).is_err());
        assert_eq!(serde_json::to_value(&lock).unwrap(), before);
    }
    let variants = [
        METADATA.replace("hyph-demo.pat.txt\n", "other.pat.txt\n"),
        format!(
            "{METADATA}\nname other\ncategory Package\nexecute AddHyphen name=other synonyms=demo file=other.tex lefthyphenmin=2 righthyphenmin=2\nrunfiles size=1\n texmf-dist/tex/other.tex\n"
        ),
        format!(
            "{METADATA}\nname other\ncategory Package\nrunfiles size=1\n texmf-dist/tex/other/hyph-demo.pat.txt\n"
        ),
        METADATA.replace("revision 1", "revision 9"),
    ];
    for metadata in variants {
        let changed = TlpdbIndex::try_parse(&metadata, Path::new("tlpkg/texlive.tlpdb")).unwrap();
        assert!(require_languages(&mut lock, &changed, &["demo".into()]).is_err());
        assert_eq!(serde_json::to_value(&lock).unwrap(), before);
    }
    // Two paths inside the same provider are ambiguous too.
    let ambiguous = METADATA.replace(" texmf-dist/tex/generic/hyph-demo/hyph-demo.pat.txt", " texmf-dist/tex/generic/hyph-demo/hyph-demo.pat.txt\n texmf-dist/tex/other/hyph-demo.pat.txt");
    assert!(
        TlpdbIndex::try_parse(&ambiguous, Path::new("tlpkg/texlive.tlpdb"))
            .unwrap()
            .resolve_languages(&["demo".into()])
            .is_err()
    );
}

#[test]
fn add_hyphen_parser_checks_metadata_and_retains_optional_and_quoted_fields() {
    let index = TlpdbIndex::try_parse(
        &METADATA
            .replace(" synonyms=Demo,demo-alias", "")
            .replace("file_patterns=hyph-demo.pat.txt", "file_patterns=")
            .replace("file_exceptions=hyph-demo.hyp.txt", "file_exceptions=")
            .replace(" name=", r" \tname="),
        Path::new("tlpkg/texlive.tlpdb"),
    )
    .unwrap();
    let manifest = index.resolve_languages(&["demo".into()]).unwrap();
    assert!(manifest.languages[0].declaration.synonyms.is_empty());
    assert_eq!(
        manifest.runtime_files(),
        ["tex/generic/hyph-demo/loadhyph-demo.tex"]
    );
    for (from, to) in [
        ("name=demo", "name=../demo"),
        ("name=demo", "name=demo name=other"),
        ("lefthyphenmin=2", "lefthyphenmin=256"),
        ("lefthyphenmin=2", "lefthyphenmin=-1"),
        ("lefthyphenmin=2", ""),
        ("file=loadhyph-demo.tex", "file=../escape.tex"),
        (
            "file_patterns=hyph-demo.pat.txt",
            "file_patterns=/absolute.txt",
        ),
        (
            "luaspecial=\"disabled:example engine restriction\"",
            "luaspecial=\"unterminated",
        ),
    ] {
        assert!(
            TlpdbIndex::try_parse(
                &METADATA.replace(from, to),
                Path::new("tlpkg/texlive.tlpdb")
            )
            .is_err(),
            "accepted {to}"
        );
    }
}

#[test]
fn verified_file_manifest_reproduces_language_lock_and_rejects_tampering() {
    let root = temporary_test_root("language-files");
    let index = TlpdbIndex::try_parse(METADATA, Path::new("tlpkg/texlive.tlpdb")).unwrap();
    let mut lock = scanned();
    resolve(&mut lock, &index);
    require_languages(&mut lock, &index, &["demo".into()]).unwrap();
    for provider in &lock.closure {
        for file in &index.package(&provider.provider).unwrap().runfiles {
            let path = root.join(file);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(&path, b"% synthetic hyphenation test bytes\n").unwrap();
        }
    }
    let store = root.join("store");
    hydrate_lock(
        &mut lock,
        &index,
        &PackageByteSource::local_texlive(&root),
        &store,
    )
    .unwrap();
    let manifest = export_file_manifest(&lock, &store).unwrap();
    manifest.validate(&lock).unwrap();
    assert_eq!(manifest.files.len(), 4);
    assert!(manifest.files.iter().all(|file| file.size == 35));
    let schema: serde_json::Value =
        serde_json::from_str(include_str!("../../schemas/pqty.files.schema.json")).unwrap();
    jsonschema::draft202012::validate(&schema, &serde_json::to_value(&manifest).unwrap()).unwrap();
    let round_trip: FileManifest =
        serde_json::from_value(serde_json::to_value(&manifest).unwrap()).unwrap();
    round_trip.validate(&lock).unwrap();
    assert_eq!(
        export_file_manifest(&lock, &store).unwrap().fingerprint,
        manifest.fingerprint
    );
    let mut changed = manifest.clone();
    changed.files.pop();
    assert!(changed.validate(&lock).is_err());
    let mut changed = manifest.clone();
    changed.files[0].digest = format!("sha256:{}", "00".repeat(32));
    assert!(
        changed
            .validate(&lock)
            .unwrap_err()
            .to_string()
            .contains("integrity")
    );
    let mut changed = manifest.clone();
    changed.files[0].size += 1;
    assert!(changed.validate(&lock).is_err());
    let mut changed = manifest.clone();
    changed.files[0].owner = "other".into();
    assert!(changed.validate(&lock).is_err());
    let mut changed = manifest.clone();
    changed.files.swap(0, 1);
    assert!(changed.validate(&lock).is_err());
    let digest = manifest.files[0].digest.strip_prefix("sha256:").unwrap();
    let object = store.join(&digest[..2]).join(digest);
    make_test_writable(&object);
    fs::write(&object, b"corrupt").unwrap();
    assert!(
        export_file_manifest(&lock, &store)
            .unwrap_err()
            .to_string()
            .contains("corrupt")
    );
    fs::remove_file(&object).unwrap();
    assert!(export_file_manifest(&lock, &store).is_err());
    make_test_tree_writable(&root);
    fs::remove_dir_all(&root).unwrap();
}
