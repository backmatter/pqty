#[test]
fn environment_manifest_reconciles_engine_neutral_traces() {
    let lock = environment_test_lock();
    let environment = ResolvedEnvironment::from_lock(&lock).unwrap();
    assert_eq!(environment.schema, ENVIRONMENT_SCHEMA);
    assert_eq!(environment.font_maps, vec!["foo.map"]);
    assert_eq!(
        environment.fingerprint,
        ResolvedEnvironment::from_lock(&lock).unwrap().fingerprint
    );

    let report = environment
        .reconcile_trace(&InputTrace {
            schema: TRACE_SCHEMA.to_string(),
            producer: Some("test-engine".to_string()),
            environment_fingerprint: Some(environment.fingerprint.clone()),
            inputs: vec![
                ObservedInput {
                    requested: "foo.sty".to_string(),
                    resolved: Some("texmf-dist/tex/latex/foo/foo.sty".to_string()),
                    scope: TraceScope::Package,
                    kind: Some(ResourceKind::Tex),
                },
                ObservedInput {
                    requested: "plain.fmt".to_string(),
                    resolved: None,
                    scope: TraceScope::Engine,
                    kind: Some(ResourceKind::Format),
                },
                ObservedInput {
                    requested: "missing.sty".to_string(),
                    resolved: None,
                    scope: TraceScope::Package,
                    kind: Some(ResourceKind::Tex),
                },
                ObservedInput {
                    requested: "foo.sty".to_string(),
                    resolved: Some("tex/latex/other/foo.sty".to_string()),
                    scope: TraceScope::Package,
                    kind: Some(ResourceKind::Tex),
                },
            ],
        })
        .unwrap();
    assert_eq!(report.matched.len(), 1);
    assert_eq!(report.matched[0].owner, "foo");
    assert_eq!(report.ignored.len(), 1);
    assert_eq!(report.missing.len(), 2);
}

fn environment_test_lock() -> LockFile {
    let files = vec![
        LockedFile {
            tds_path: "tex/latex/foo/foo.sty".to_string(),
            kind: ResourceKind::Tex,
        },
        LockedFile {
            tds_path: "fonts/map/dvips/foo/foo.map".to_string(),
            kind: ResourceKind::Map,
        },
    ];
    let integrity = format!("sha256-{}", BASE64.encode([3_u8; 32]));
    LockFile {
        schema: LOCK_SCHEMA.to_string(),
        stage: LockStage::Exact,
        generated_with: "test".to_string(),
        environment: Environment::default(),
        registries: vec![Registry {
            id: "test".to_string(),
            kind: RegistryKind::Tlnet,
            url: "https://example.invalid/tlnet".to_string(),
            snapshot: Some("2026-07-26".to_string()),
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
            satisfies: vec!["foo".to_string()],
            integrity: Some(integrity),
            dependencies: Vec::new(),
            direct: true,
            requested_by: Vec::new(),
            store_key: Some(format!("sha256:{}", "03".repeat(32))),
            engines: Vec::new(),
            font_maps: vec!["foo.map".to_string()],
            runtime_requests: Vec::new(),
            files,
        }],
        unresolved: Vec::new(),
    }
}

#[test]
fn property_trace_reconciliation_partitions_generated_input_orders() {
    let lock: LockFile =
        serde_json::from_str(include_str!("../../tests/golden/v1/lock-exact.json")).unwrap();
    let environment = ResolvedEnvironment::from_lock(&lock).unwrap();
    let inputs = vec![
        ObservedInput {
            requested: "foo.sty".to_string(),
            resolved: Some("tex/latex/foo/foo.sty".to_string()),
            scope: TraceScope::Package,
            kind: Some(ResourceKind::Tex),
        },
        ObservedInput {
            requested: "missing.sty".to_string(),
            resolved: None,
            scope: TraceScope::Package,
            kind: Some(ResourceKind::Tex),
        },
        ObservedInput {
            requested: "main.tex".to_string(),
            resolved: Some("main.tex".to_string()),
            scope: TraceScope::Project,
            kind: Some(ResourceKind::Tex),
        },
    ];
    for rotation in 0..inputs.len() {
        let mut generated = inputs.clone();
        generated.rotate_left(rotation);
        let report = environment
            .reconcile_trace(&InputTrace {
                schema: TRACE_SCHEMA.to_string(),
                producer: Some("property-test".to_string()),
                environment_fingerprint: Some(environment.fingerprint.clone()),
                inputs: generated,
            })
            .unwrap();
        assert_eq!(
            report.matched.len() + report.missing.len() + report.ignored.len(),
            inputs.len()
        );
        assert_eq!(report.matched.len(), 1);
        assert_eq!(report.matched[0].owner, "foo");
        assert_eq!(report.missing.len(), 1);
        assert_eq!(report.ignored.len(), 1);
    }
}

#[test]
fn trace_convergence_adds_and_hydrates_a_deterministic_provider_closure() {
    let (root, index, mut lock) = convergence_fixture();
    let (reloaded, registry_url) = load_convergence_index(&lock, None, None, false, false).unwrap();
    assert!(registry_url.is_none());
    assert_eq!(reloaded.registry(), index.registry());
    let previous = ResolvedEnvironment::from_lock(&lock).unwrap().fingerprint;
    let mut trace = InputTrace {
        schema: TRACE_SCHEMA.to_string(),
        producer: Some("test-engine".to_string()),
        environment_fingerprint: Some(previous.clone()),
        inputs: vec![
            ObservedInput {
                requested: "alpha.sty".to_string(),
                resolved: Some("tex/latex/alpha/alpha.sty".to_string()),
                scope: TraceScope::Package,
                kind: Some(ResourceKind::Tex),
            },
            ObservedInput {
                requested: "beta.sty".to_string(),
                resolved: Some("texmf-dist/tex/latex/beta/beta.sty".to_string()),
                scope: TraceScope::Package,
                kind: Some(ResourceKind::Tex),
            },
            ObservedInput {
                requested: "pdflatex.fmt".to_string(),
                resolved: None,
                scope: TraceScope::Engine,
                kind: Some(ResourceKind::Format),
            },
        ],
    };

    let report = converge_trace(
        &mut lock,
        &trace,
        &index,
        &PackageByteSource::local_texlive(&root),
        &root.join("store"),
    )
    .unwrap();
    assert_eq!(report.status, ConvergenceStatus::Changed);
    assert_eq!(report.previous_environment_fingerprint, previous);
    assert_ne!(report.environment_fingerprint, previous);
    assert_eq!(report.added_providers, vec!["beta", "gamma"]);
    assert!(report.unresolved.is_empty());
    assert_eq!(report.matched.len(), 2);
    assert_eq!(report.ignored.len(), 1);

    let beta = lock
        .closure
        .iter()
        .find(|entry| entry.provider == "beta")
        .unwrap();
    assert!(beta.direct);
    assert_eq!(beta.runtime_requests, vec!["beta.sty"]);
    assert!(beta.integrity.is_some());
    let gamma = lock
        .closure
        .iter()
        .find(|entry| entry.provider == "gamma")
        .unwrap();
    assert_eq!(gamma.requested_by, vec!["beta"]);
    assert!(
        lock.closure
            .iter()
            .all(|entry| entry.provider != "engine-core")
    );

    let stale = converge_trace(
        &mut lock,
        &trace,
        &index,
        &PackageByteSource::local_texlive(&root),
        &root.join("store"),
    )
    .unwrap_err();
    assert!(
        stale
            .to_string()
            .contains("trace was produced by environment")
    );

    trace.environment_fingerprint = Some(report.environment_fingerprint.clone());
    let stable_report = converge_trace(
        &mut lock,
        &trace,
        &index,
        &PackageByteSource::local_texlive(&root),
        &root.join("store"),
    )
    .unwrap();
    assert_eq!(stable_report.status, ConvergenceStatus::Stable);
    assert!(stable_report.added_providers.is_empty());
    assert_eq!(
        stable_report.environment_fingerprint,
        report.environment_fingerprint
    );
    let _ = fs::remove_dir_all(root);
}

#[test]
fn trace_convergence_case_folds_fontspec_generated_filenames() {
    let (root, index, mut lock) = convergence_fixture();
    let previous = ResolvedEnvironment::from_lock(&lock).unwrap().fingerprint;
    let mut trace = InputTrace {
        schema: TRACE_SCHEMA.to_string(),
        producer: Some("fontspec".to_string()),
        environment_fingerprint: Some(previous),
        inputs: vec![ObservedInput {
            requested: "TeXGyreHeros-Regular.otf".to_string(),
            resolved: None,
            scope: TraceScope::Package,
            kind: Some(ResourceKind::OpenTypeFont),
        }],
    };

    let report = converge_trace(
        &mut lock,
        &trace,
        &index,
        &PackageByteSource::local_texlive(&root),
        &root.join("store"),
    )
    .unwrap();
    assert_eq!(report.status, ConvergenceStatus::Changed);
    assert_eq!(report.added_providers, vec!["tex-gyre"]);
    assert_eq!(
        lock.closure
            .iter()
            .find(|entry| entry.provider == "tex-gyre")
            .unwrap()
            .runtime_requests,
        vec!["texgyreheros-regular.otf"]
    );

    trace.environment_fingerprint = Some(report.environment_fingerprint);
    let stable = converge_trace(
        &mut lock,
        &trace,
        &index,
        &PackageByteSource::local_texlive(&root),
        &root.join("store"),
    )
    .unwrap();
    assert_eq!(stable.status, ConvergenceStatus::Stable);
    assert_eq!(stable.matched[0].requested, "TeXGyreHeros-Regular.otf");
    assert_eq!(stable.matched[0].owner, "tex-gyre");
    let _ = fs::remove_dir_all(root);
}

#[test]
fn trace_convergence_resolves_font_families_across_outline_formats() {
    let (root, index, mut lock) = convergence_fixture();
    let previous = ResolvedEnvironment::from_lock(&lock).unwrap().fingerprint;
    let mut trace = InputTrace {
        schema: TRACE_SCHEMA.to_string(),
        producer: Some("fontspec".to_string()),
        environment_fingerprint: Some(previous),
        inputs: vec![ObservedInput {
            requested: "Lato-Regular".to_string(),
            resolved: None,
            scope: TraceScope::Package,
            kind: Some(ResourceKind::FontFamily),
        }],
    };

    let report = converge_trace(
        &mut lock,
        &trace,
        &index,
        &PackageByteSource::local_texlive(&root),
        &root.join("store"),
    )
    .unwrap();
    assert_eq!(report.status, ConvergenceStatus::Changed);
    assert_eq!(report.added_providers, vec!["lato"]);
    assert_eq!(
        lock.closure
            .iter()
            .find(|entry| entry.provider == "lato")
            .unwrap()
            .runtime_requests,
        vec!["Lato-Regular.ttf"]
    );

    trace.environment_fingerprint = Some(report.environment_fingerprint);
    let stable = converge_trace(
        &mut lock,
        &trace,
        &index,
        &PackageByteSource::local_texlive(&root),
        &root.join("store"),
    )
    .unwrap();
    assert_eq!(stable.status, ConvergenceStatus::Stable);
    assert_eq!(stable.matched[0].requested, "Lato-Regular");
    assert_eq!(stable.matched[0].owner, "lato");
    let _ = fs::remove_dir_all(root);
}

#[test]
fn unresolved_trace_convergence_does_not_partially_change_the_lock() {
    let (root, index, mut lock) = convergence_fixture();
    let before = serde_json::to_vec(&lock).unwrap();
    let trace = InputTrace {
        schema: TRACE_SCHEMA.to_string(),
        producer: None,
        environment_fingerprint: None,
        inputs: vec![
            ObservedInput {
                requested: "beta.sty".to_string(),
                resolved: None,
                scope: TraceScope::Package,
                kind: Some(ResourceKind::Tex),
            },
            ObservedInput {
                requested: "unknown.sty".to_string(),
                resolved: None,
                scope: TraceScope::Package,
                kind: Some(ResourceKind::Tex),
            },
        ],
    };

    let report = converge_trace(
        &mut lock,
        &trace,
        &index,
        &PackageByteSource::local_texlive(&root),
        &root.join("store"),
    )
    .unwrap();
    assert_eq!(report.status, ConvergenceStatus::Unresolved);
    assert!(report.added_providers.is_empty());
    assert_eq!(report.unresolved.len(), 1);
    assert_eq!(
        report.unresolved[0].reason,
        ConvergenceUnresolvedReason::NoProvider
    );
    assert_eq!(serde_json::to_vec(&lock).unwrap(), before);
    let _ = fs::remove_dir_all(root);
}

#[test]
fn trace_convergence_rejects_different_registry_metadata() {
    let (root, _index, mut lock) = convergence_fixture();
    let changed_metadata = concat!(
        "name alpha\n",
        "category Package\n",
        "revision 999\n",
        "runfiles size=1\n",
        " texmf-dist/tex/latex/alpha/alpha.sty\n",
        "\n",
        "name beta\n",
        "category Package\n",
        "revision 999\n",
        "runfiles size=1\n",
        " texmf-dist/tex/latex/beta/beta.sty\n",
    );
    let changed = TlpdbIndex::parse(changed_metadata, &root.join("tlpkg/texlive.tlpdb"));
    let before = serde_json::to_vec(&lock).unwrap();
    let trace = InputTrace {
        schema: TRACE_SCHEMA.to_string(),
        producer: None,
        environment_fingerprint: None,
        inputs: vec![ObservedInput {
            requested: "beta.sty".to_string(),
            resolved: None,
            scope: TraceScope::Package,
            kind: Some(ResourceKind::Tex),
        }],
    };

    let error = converge_trace(
        &mut lock,
        &trace,
        &changed,
        &PackageByteSource::local_texlive(&root),
        &root.join("store"),
    )
    .unwrap_err();
    assert!(error.to_string().contains("does not match"));
    assert_eq!(serde_json::to_vec(&lock).unwrap(), before);
    let _ = fs::remove_dir_all(root);
}
use std::fs;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64;

use crate::tests::convergence_fixture;
use crate::{
    ConsumerRequirements, ConvergenceStatus, ConvergenceUnresolvedReason, ENVIRONMENT_SCHEMA,
    Environment, InputTrace, LOCK_SCHEMA, LockFile, LockStage, LockedFile, ObservedInput,
    PackageByteSource, PackageRegistry, PackageSource, PackageSourceKind, Registry, RegistryKind,
    ResolvedEnvironment, ResolvedPackage, ResourceKind, SourceRecord, TRACE_SCHEMA, TlpdbIndex,
    TraceScope, converge_trace, load_convergence_index,
};
