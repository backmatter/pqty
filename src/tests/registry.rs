#[test]
fn parses_tlpdb_and_resolves_provider() {
    let sample = concat!(
        "name graphics\n",
        "category Package\n",
        "revision 75374\n",
        "depend graphics-cfg\n",
        "execute addMixedMap graphics.map\n",
        "containerchecksum ",
        "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
        "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
        "\n",
        "containersize 4096\n",
        "runfiles size=36\n",
        " texmf-dist/tex/latex/graphics/graphicx.sty\n",
        " texmf-dist/tex/latex/graphics/graphics.sty\n",
        "\n",
        "name graphics-cfg\n",
        "category Package\n",
        "revision 123\n",
        "runfiles size=1\n",
        " texmf-dist/tex/latex/graphics-cfg/graphics.cfg\n",
    );
    let index = TlpdbIndex::parse(sample, Path::new("/usr/share/tlpkg/texlive.tlpdb"));
    assert_eq!(index.provider_of("graphicx", &["sty"]), Some("graphics"));
    assert_eq!(index.provider_of("graphics", &["cls"]), None);
    let meta = index.package("graphics").unwrap();
    assert_eq!(meta.version, "tlrev:75374");
    assert_eq!(meta.depends, vec!["graphics-cfg"]);
    assert_eq!(meta.font_maps, vec!["graphics.map"]);
    assert_eq!(
        meta.container_checksum.as_deref(),
        Some(concat!(
            "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
        ))
    );
    assert_eq!(meta.container_size, Some(4096));
    // A package without container metadata (installed-style) stays None.
    assert_eq!(
        index.package("graphics-cfg").unwrap().container_checksum,
        None
    );
    assert_eq!(
        index.provider_of_path("tex/latex/graphics/graphicx.sty"),
        Some("graphics")
    );
    assert_eq!(index.metadata_digest(), digest_bytes(sample.as_bytes()));
}

#[test]
fn rejects_adversarial_tlpdb_records_before_indexing() {
    let source = Path::new("/tmp/tlpkg/texlive.tlpdb");
    for (label, metadata) in [
        (
            "provider traversal",
            include_str!("../../tests/fixtures/adversarial/unsafe-provider.tlpdb"),
        ),
        (
            "provider separator",
            "name bad/name\nrunfiles size=1\n RELOC/tex/a.sty\n",
        ),
        (
            "absolute runfile",
            "name safe\nrunfiles size=1\n /etc/passwd\n",
        ),
        (
            "runfile traversal",
            include_str!("../../tests/fixtures/adversarial/unsafe-runfile.tlpdb"),
        ),
        (
            "windows separator",
            "name safe\nrunfiles size=1\n RELOC\\tex\\escape.sty\n",
        ),
        ("malformed size", "name safe\ncontainersize many\n"),
        (
            "malformed checksum",
            include_str!("../../tests/fixtures/adversarial/malformed-container.tlpdb"),
        ),
        (
            "incomplete container identity",
            "name safe\ncontainersize 1\n",
        ),
        ("duplicate provider", "name safe\n\nname safe\n"),
        (
            "duplicate runfile",
            include_str!("../../tests/fixtures/adversarial/duplicate-runfile.tlpdb"),
        ),
    ] {
        assert!(
            TlpdbIndex::try_parse(metadata, source).is_err(),
            "{label} was accepted"
        );
    }
}

#[test]
fn rejects_malformed_artifact_fixture() {
    let text = include_str!("../../tests/fixtures/adversarial/malformed-lock.json");
    assert!(serde_json::from_str::<LockFile>(text).is_err());
}

#[test]
fn generic_runtime_requirements_extend_the_exact_closure() {
    let sample = concat!(
        "name alpha\n",
        "category Package\n",
        "revision 1\n",
        "depend firstaid\n",
        "runfiles size=1\n",
        " RELOC/tex/latex/alpha/alpha.sty\n",
        "\n",
        "name firstaid\n",
        "category Package\n",
        "revision 2\n",
        "runfiles size=1\n",
        " RELOC/tex/latex/firstaid/latex2e-first-aid-for-external-files.ltx\n",
    );
    let index = TlpdbIndex::parse(sample, Path::new("/tmp/tlpkg/texlive.tlpdb"));
    let mut source = MemorySourceTree::default();
    source.insert(
        VirtualPath::new("main.tex").expect("valid path"),
        b"Hello".as_slice(),
    );
    let mut lock = scan_source(&source, VirtualPath::new("main.tex").expect("valid path"))
        .expect("source scans");
    resolve(&mut lock, &index);
    require_runtime(
        &mut lock,
        &index,
        &["texmf-dist/tex/latex/alpha/alpha.sty".to_string()],
        &["firstaid".to_string()],
    )
    .expect("requirements resolve");

    let providers = lock
        .closure
        .iter()
        .map(|entry| entry.provider.as_str())
        .collect::<BTreeSet<_>>();
    assert_eq!(providers, BTreeSet::from(["alpha", "firstaid"]));
    let alpha = lock
        .closure
        .iter()
        .find(|entry| entry.provider == "alpha")
        .expect("alpha entry");
    assert!(alpha.direct);
    assert_eq!(alpha.runtime_requests, vec!["tex/latex/alpha/alpha.sty"]);
    assert_eq!(
        lock.consumer_requirements,
        ConsumerRequirements {
            providers: vec!["firstaid".to_string()],
            files: vec!["tex/latex/alpha/alpha.sty".to_string()],
        }
    );
}

#[test]
fn generic_runtime_requirements_reject_absolute_file_requests() {
    let index = TlpdbIndex::parse(
        "name foo\nrunfiles size=1\n texmf-dist/tex/latex/foo/foo.sty\n",
        Path::new("/tmp/tlpkg/texlive.tlpdb"),
    );
    let mut source = MemorySourceTree::default();
    source.insert(VirtualPath::new("main.tex").unwrap(), b"Hello".as_slice());
    let mut lock = scan_source(&source, VirtualPath::new("main.tex").unwrap()).unwrap();
    resolve(&mut lock, &index);
    let error = require_runtime(
        &mut lock,
        &index,
        &["/tex/latex/foo/foo.sty".to_string()],
        &[],
    )
    .unwrap_err();
    assert!(error.to_string().contains("Portable Path"));
}

#[test]
fn stable_provider_wins_over_latex_dev_duplicate() {
    let sample = concat!(
        "name latex-tools-dev\n",
        "revision 2\n",
        "runfiles size=1\n",
        " texmf-dist/tex/latex/latex-dev/array.sty\n",
        "\n",
        "name tools\n",
        "revision 1\n",
        "runfiles size=1\n",
        " texmf-dist/tex/latex/tools/array.sty\n",
    );
    let index = TlpdbIndex::parse(sample, Path::new("/usr/share/tlpkg/texlive.tlpdb"));
    assert_eq!(index.provider_of_file("array.sty"), Some("tools"));
}

#[test]
fn stable_duplicate_basename_is_ambiguous_but_exact_path_is_not() {
    let sample = concat!(
        "name arabtex\n",
        "category Package\n",
        "revision 1\n",
        "runfiles size=1\n",
        " RELOC/tex/latex/arabtex/afoot.sty\n",
        "\n",
        "name ledmac\n",
        "category Package\n",
        "revision 2\n",
        "runfiles size=1\n",
        " RELOC/tex/latex/ledmac/afoot.sty\n",
    );
    let index = TlpdbIndex::parse(sample, Path::new("/usr/share/tlpkg/texlive.tlpdb"));
    assert_eq!(
        index.providers_of_file("afoot.sty"),
        vec!["arabtex", "ledmac"]
    );
    assert_eq!(index.provider_of_file("afoot.sty"), None);
    assert_eq!(
        index.provider_of_path("tex/latex/arabtex/afoot.sty"),
        Some("arabtex")
    );

    let input = ObservedInput {
        requested: "afoot.sty".to_string(),
        resolved: Some("tex/latex/arabtex/afoot.sty".to_string()),
        scope: TraceScope::Package,
        kind: Some(ResourceKind::Tex),
    };
    let (_, request, providers) = trace_provider_candidates(&index, &input).unwrap();
    assert_eq!(request, "afoot.sty");
    assert_eq!(providers, vec!["arabtex"]);
}

#[test]
fn package_file_resolution_ignores_engine_internal_duplicates() {
    let sample = concat!(
        "name texlive-scripts\n",
        "category TLCore\n",
        "revision 1\n",
        "runfiles size=1\n",
        " tlpkg/tlgs/Resource/Font/putb8a.pfb\n",
        "\n",
        "name utopia\n",
        "category Package\n",
        "revision 2\n",
        "runfiles size=1\n",
        " RELOC/fonts/type1/adobe/utopia/putb8a.pfb\n",
        "\n",
        "name koma-script\n",
        "category TLCore\n",
        "revision 3\n",
        "runfiles size=1\n",
        " RELOC/tex/latex/koma-script/scrbook.cls\n",
    );
    let index = TlpdbIndex::parse(sample, Path::new("/usr/share/tlpkg/texlive.tlpdb"));

    assert_eq!(index.providers_of_file("putb8a.pfb"), vec!["utopia"]);
    assert_eq!(index.provider_of_file("putb8a.pfb"), Some("utopia"));
    assert_eq!(
        index.providers_of_path("tlpkg/tlgs/Resource/Font/putb8a.pfb"),
        Vec::<&str>::new()
    );
    assert_eq!(index.providers_of("scrbook", &["cls"]), vec!["koma-script"]);
}

#[test]
fn resolve_builds_transitive_closure() {
    let sample = concat!(
        "name graphics\n",
        "revision 75374\n",
        "depend graphics-cfg\n",
        "runfiles size=1\n",
        " texmf-dist/tex/latex/graphics/graphicx.sty\n",
        "\n",
        "name graphics-cfg\n",
        "revision 123\n",
        "runfiles size=1\n",
        " texmf-dist/tex/latex/graphics-cfg/graphics.cfg\n",
    );
    let index = TlpdbIndex::parse(sample, Path::new("/usr/share/tlpkg/texlive.tlpdb"));
    let mut lock = LockFile {
        schema: LOCK_SCHEMA.to_string(),
        stage: LockStage::Scanned,
        generated_with: "test".to_string(),
        environment: Environment::default(),
        registries: Vec::new(),
        consumer_requirements: ConsumerRequirements::default(),
        root: "main.tex".to_string(),
        sources: Vec::new(),
        document_class: None,
        loaded_classes: Vec::new(),
        packages: vec![PackageRecord {
            name: "graphicx".to_string(),
            options: Vec::new(),
            command: "usepackage".to_string(),
            resolved_path: None,
            source: Location {
                path: "main.tex".to_string(),
                line: 1,
            },
        }],
        inputs: Vec::new(),
        bibliographies: Vec::new(),
        graphics: Vec::new(),
        closure: Vec::new(),
        unresolved: Vec::new(),
    };
    resolve(&mut lock, &index);
    assert_eq!(lock.closure.len(), 2);
    let direct = lock
        .closure
        .iter()
        .find(|p| p.provider == "graphics")
        .unwrap();
    assert!(direct.direct);
    assert_eq!(direct.satisfies, vec!["graphicx"]);
    let transitive = lock
        .closure
        .iter()
        .find(|p| p.provider == "graphics-cfg")
        .unwrap();
    assert!(!transitive.direct);
    assert_eq!(transitive.requested_by, vec!["graphics"]);
}

#[test]
fn normalizes_runfile_prefixes() {
    // Relocated (RELOC), non-relocated/installed (texmf-dist), and bare TDS
    // forms all canonicalize to the same placement + bare path.
    let reloc = normalize_runfile("RELOC/tex/latex/amsmath/amsmath.sty");
    let dist = normalize_runfile("texmf-dist/tex/latex/amsmath/amsmath.sty");
    assert_eq!(reloc, dist);
    assert_eq!(reloc.0, "texmf-dist/tex/latex/amsmath/amsmath.sty");
    assert_eq!(reloc.1, "tex/latex/amsmath/amsmath.sty");
}

#[test]
fn property_portable_tds_paths_and_registry_normalization_are_prefix_invariant() {
    let components = ["a", "A0", "with space", "naïve", "x_y-z"];
    for first in components {
        for second in components {
            let path = format!("{first}/{second}/file.sty");
            assert!(validate_portable_path(&path, "generated path").is_ok());
            assert!(validate_tds_path(&path, "generated TDS path").is_ok());
            let bare = normalize_runfile(&path);
            let reloc = normalize_runfile(&format!("RELOC/{path}"));
            let dist = normalize_runfile(&format!("texmf-dist/{path}"));
            assert_eq!(bare, reloc);
            assert_eq!(bare, dist);
        }
    }

    for invalid in [
        "",
        "/absolute",
        "C:/drive/path",
        "../parent",
        "a/../parent",
        "a//empty",
        "a/./current",
        r"a\windows",
        "a/\0/control",
    ] {
        assert!(validate_portable_path(invalid, "generated path").is_err());
        assert!(validate_tds_path(invalid, "generated TDS path").is_err());
    }
    for invalid_tds in ["tex/a:b.sty", "RELOC/tex/a:b.sty"] {
        assert!(validate_tds_path(invalid_tds, "generated TDS path").is_err());
    }
}

#[test]
fn property_provider_identifiers_accept_only_url_and_path_safe_alphabet() {
    for valid in [
        "a",
        "00texlive.config",
        "latex-tools-dev",
        "tex4ht",
        "a_b+c",
    ] {
        assert!(validate_provider_identifier(valid, "generated provider").is_ok());
    }
    for invalid in [
        "",
        ".",
        "..",
        "../escape",
        "with/slash",
        r"with\slash",
        "with space",
        "control\nname",
        "naïve",
    ] {
        assert!(validate_provider_identifier(invalid, "generated provider").is_err());
    }
}

#[test]
fn reads_relocated_and_non_relocated_container_paths() {
    let bytes = b"package".to_vec();
    let mut relocated =
        BTreeMap::from([("tex/latex/amsmath/amsmath.sty".to_string(), bytes.clone())]);
    assert_eq!(
        take_container_runfile(&mut relocated, "RELOC/tex/latex/amsmath/amsmath.sty"),
        Some(bytes.clone())
    );

    let mut non_relocated = BTreeMap::from([(
        "texmf-dist/scripts/luaotfload/luaotfload-tool.lua".to_string(),
        bytes.clone(),
    )]);
    assert_eq!(
        take_container_runfile(
            &mut non_relocated,
            "texmf-dist/scripts/luaotfload/luaotfload-tool.lua"
        ),
        Some(bytes)
    );
}

#[test]
fn derives_tlnet_base() {
    assert_eq!(
        tlnet_base_from_tlpdb_url("https://x/tlnet/tlpkg/texlive.tlpdb.xz").as_deref(),
        Some("https://x/tlnet")
    );
    assert_eq!(
        tlnet_base_from_tlpdb_url("https://x/tlnet/tlpkg/texlive.tlpdb").as_deref(),
        Some("https://x/tlnet")
    );
    assert_eq!(tlnet_base_from_tlpdb_url("https://x/other.tlpdb"), None);
}

#[test]
fn bounded_reads_and_decompression_writes_reject_oversized_artifacts() {
    assert_eq!(read_bounded(&b"abcd"[..], 4, "fixture").unwrap(), b"abcd");
    assert!(read_bounded(&b"abcde"[..], 4, "fixture").is_err());

    let mut writer = BoundedWriter::new(4, "fixture");
    writer.write_all(b"abcd").unwrap();
    assert!(writer.write_all(b"e").is_err());
}
use std::collections::{BTreeMap, BTreeSet};
use std::io::Write as _;
use std::path::Path;

use crate::{
    BoundedWriter, ConsumerRequirements, Environment, LOCK_SCHEMA, Location, LockFile, LockStage,
    MemorySourceTree, ObservedInput, PackageRecord, PackageRegistry, ResourceKind, TlpdbIndex,
    TraceScope, VirtualPath, digest_bytes, normalize_runfile, read_bounded, require_runtime,
    resolve, scan_source, take_container_runfile, tlnet_base_from_tlpdb_url,
    trace_provider_candidates, validate_portable_path, validate_provider_identifier,
    validate_tds_path,
};
