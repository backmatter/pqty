#[test]
fn machine_readable_schemas_match_the_rust_protocol_constants() {
    for (text, expected_id, expected_const) in [
        (
            include_str!("../../schemas/pqty.capabilities.schema.json"),
            "https://raw.githubusercontent.com/backmatter/pqty/main/schemas/pqty.capabilities.schema.json",
            "pqty.capabilities/v1",
        ),
        (
            include_str!("../../schemas/pqty.lock.schema.json"),
            "https://raw.githubusercontent.com/backmatter/pqty/main/schemas/pqty.lock.schema.json",
            LOCK_SCHEMA,
        ),
        (
            include_str!("../../schemas/pqty.env.schema.json"),
            "https://raw.githubusercontent.com/backmatter/pqty/main/schemas/pqty.env.schema.json",
            ENVIRONMENT_SCHEMA,
        ),
        (
            include_str!("../../schemas/pqty.trace.schema.json"),
            "https://raw.githubusercontent.com/backmatter/pqty/main/schemas/pqty.trace.schema.json",
            TRACE_SCHEMA,
        ),
        (
            include_str!("../../schemas/pqty.trace-report.schema.json"),
            "https://raw.githubusercontent.com/backmatter/pqty/main/schemas/pqty.trace-report.schema.json",
            TRACE_REPORT_SCHEMA,
        ),
        (
            include_str!("../../schemas/pqty.convergence.schema.json"),
            "https://raw.githubusercontent.com/backmatter/pqty/main/schemas/pqty.convergence.schema.json",
            CONVERGENCE_REPORT_SCHEMA,
        ),
    ] {
        let schema: serde_json::Value = serde_json::from_str(text).unwrap();
        assert_eq!(schema["$id"], expected_id);
        assert_eq!(schema["properties"]["schema"]["const"], expected_const);
    }
    let progress: serde_json::Value =
        serde_json::from_str(include_str!("../../schemas/pqty.progress.schema.json")).unwrap();
    assert_eq!(
        progress["$id"],
        "https://raw.githubusercontent.com/backmatter/pqty/main/schemas/pqty.progress.schema.json"
    );
    for variant in progress["oneOf"].as_array().expect("progress variants") {
        assert_eq!(variant["properties"]["schema"]["const"], PROGRESS_SCHEMA);
    }
}

#[test]
fn machine_protocol_schemas_are_closed_and_validate_golden_artifacts() {
    let cases = [
        (
            include_str!("../../schemas/pqty.capabilities.schema.json"),
            include_str!("../../tests/golden/v1/capabilities.json"),
        ),
        (
            include_str!("../../schemas/pqty.lock.schema.json"),
            include_str!("../../tests/golden/v1/lock-scanned.json"),
        ),
        (
            include_str!("../../schemas/pqty.lock.schema.json"),
            include_str!("../../tests/golden/v1/lock-resolved.json"),
        ),
        (
            include_str!("../../schemas/pqty.lock.schema.json"),
            include_str!("../../tests/golden/v1/lock-exact.json"),
        ),
        (
            include_str!("../../schemas/pqty.env.schema.json"),
            include_str!("../../tests/golden/v1/environment.json"),
        ),
        (
            include_str!("../../schemas/pqty.trace.schema.json"),
            include_str!("../../tests/golden/v1/trace.json"),
        ),
        (
            include_str!("../../schemas/pqty.trace-report.schema.json"),
            include_str!("../../tests/golden/v1/trace-report.json"),
        ),
        (
            include_str!("../../schemas/pqty.convergence.schema.json"),
            include_str!("../../tests/golden/v1/convergence-report.json"),
        ),
        (
            include_str!("../../schemas/pqty.progress.schema.json"),
            include_str!("../../tests/golden/v1/progress-download-plan.json"),
        ),
        (
            include_str!("../../schemas/pqty.progress.schema.json"),
            include_str!("../../tests/golden/v1/progress-download-start.json"),
        ),
        (
            include_str!("../../schemas/pqty.progress.schema.json"),
            include_str!("../../tests/golden/v1/progress-download-progress.json"),
        ),
        (
            include_str!("../../schemas/pqty.progress.schema.json"),
            include_str!("../../tests/golden/v1/progress-download-complete.json"),
        ),
    ];

    let mut checked_schemas = BTreeSet::new();
    for (schema_text, artifact_text) in cases {
        let schema: serde_json::Value = serde_json::from_str(schema_text).unwrap();
        if checked_schemas.insert(schema["$id"].as_str().unwrap().to_string()) {
            jsonschema::draft202012::meta::validate(&schema)
                .unwrap_or_else(|error| panic!("invalid JSON Schema: {error}"));
            assert_closed_objects(&schema, "#");
        }
        let artifact: serde_json::Value = serde_json::from_str(artifact_text).unwrap();
        jsonschema::draft202012::validate(&schema, &artifact)
            .unwrap_or_else(|error| panic!("golden artifact violates its schema: {error}"));
    }
}

fn assert_closed_objects(value: &serde_json::Value, path: &str) {
    match value {
        serde_json::Value::Object(object) => {
            if object.get("type").and_then(serde_json::Value::as_str) == Some("object") {
                assert_eq!(
                    object.get("additionalProperties"),
                    Some(&serde_json::Value::Bool(false)),
                    "object schema at {path} is open"
                );
            }
            for (key, nested) in object {
                assert_closed_objects(nested, &format!("{path}/{key}"));
            }
        }
        serde_json::Value::Array(values) => {
            for (index, nested) in values.iter().enumerate() {
                assert_closed_objects(nested, &format!("{path}/{index}"));
            }
        }
        _ => {}
    }
}

#[test]
fn golden_v1_artifacts_are_real_outputs_and_remain_readable() {
    let fixture_root = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/golden/v1");
    let golden_scanned = read_lock(&fixture_root.join("lock-scanned.json")).unwrap();
    let golden_resolved = read_lock(&fixture_root.join("lock-resolved.json")).unwrap();
    let golden_exact = read_lock(&fixture_root.join("lock-exact.json")).unwrap();
    assert_eq!(golden_scanned.stage, LockStage::Scanned);
    assert_eq!(golden_resolved.stage, LockStage::Resolved);
    assert_eq!(golden_exact.stage, LockStage::Exact);

    let capabilities = serde_json::to_value(crate::cli::protocol::Capabilities::current()).unwrap();
    let golden_capabilities: serde_json::Value =
        serde_json::from_str(include_str!("../../tests/golden/v1/capabilities.json")).unwrap();
    assert_eq!(capabilities, golden_capabilities);

    let mut source = MemorySourceTree::default();
    source.insert(
        VirtualPath::new("main.tex").unwrap(),
        b"\\usepackage{foo}\n",
    );
    let scanned = scan_source(&source, VirtualPath::new("main.tex").unwrap()).unwrap();
    assert_eq!(
        serde_json::to_value(&scanned).unwrap(),
        serde_json::to_value(&golden_scanned).unwrap()
    );

    let root = temporary_test_root("golden-emission");
    let metadata = concat!(
        "name 00texlive.config\n",
        "category TLCore\n",
        "depend release/2026\n",
        "\n",
        "name foo\n",
        "category Package\n",
        "revision 1\n",
        "runfiles size=1\n",
        " texmf-dist/tex/latex/foo/foo.sty\n",
    );
    let mut index = TlpdbIndex::parse(metadata, &root.join("tlpkg/texlive.tlpdb"));
    index.origin = Some("https://example.invalid/tlnet".to_string());
    let mut resolved = scanned;
    resolve(&mut resolved, &index);
    assert_eq!(
        serde_json::to_value(&resolved).unwrap(),
        serde_json::to_value(&golden_resolved).unwrap()
    );

    let package_path = root.join("texmf-dist/tex/latex/foo/foo.sty");
    fs::create_dir_all(package_path.parent().unwrap()).unwrap();
    fs::write(
        &package_path,
        include_bytes!("../../tests/golden/v1/foo.sty"),
    )
    .unwrap();
    let mut exact = resolved;
    hydrate_lock(
        &mut exact,
        &index,
        &PackageByteSource::local_texlive(&root),
        &root.join("store"),
    )
    .unwrap();
    assert_eq!(
        serde_json::to_value(&exact).unwrap(),
        serde_json::to_value(&golden_exact).unwrap()
    );

    let environment = ResolvedEnvironment::from_lock(&exact).unwrap();
    let golden_environment: serde_json::Value =
        serde_json::from_str(include_str!("../../tests/golden/v1/environment.json")).unwrap();
    assert_eq!(
        serde_json::to_value(&environment).unwrap(),
        golden_environment
    );

    let trace = read_trace(&fixture_root.join("trace.json")).unwrap();
    let trace_report = environment.reconcile_trace(&trace).unwrap();
    let golden_trace_report: serde_json::Value =
        serde_json::from_str(include_str!("../../tests/golden/v1/trace-report.json")).unwrap();
    assert_eq!(
        serde_json::to_value(trace_report).unwrap(),
        golden_trace_report
    );
    let report = stable_convergence_report(&exact, &trace)
        .unwrap()
        .expect("golden trace is stable");
    let golden_report: serde_json::Value = serde_json::from_str(include_str!(
        "../../tests/golden/v1/convergence-report.json"
    ))
    .unwrap();
    assert_eq!(serde_json::to_value(report).unwrap(), golden_report);
    let _ = fs::remove_dir_all(root);
}

#[test]
fn artifact_deserializers_reject_unknown_fields() {
    let mut lock: serde_json::Value =
        serde_json::from_str(include_str!("../../tests/golden/v1/lock-exact.json")).unwrap();
    lock["closure"][0]["surprise"] = serde_json::json!(true);
    assert!(serde_json::from_value::<LockFile>(lock).is_err());

    let mut environment: serde_json::Value =
        serde_json::from_str(include_str!("../../tests/golden/v1/environment.json")).unwrap();
    environment["packages"][0]["surprise"] = serde_json::json!(true);
    assert!(serde_json::from_value::<ResolvedEnvironment>(environment).is_err());

    let mut trace: serde_json::Value =
        serde_json::from_str(include_str!("../../tests/golden/v1/trace.json")).unwrap();
    trace["inputs"][0]["surprise"] = serde_json::json!(true);
    assert!(serde_json::from_value::<InputTrace>(trace).is_err());

    let mut trace_report: serde_json::Value =
        serde_json::from_str(include_str!("../../tests/golden/v1/trace-report.json")).unwrap();
    trace_report["missing"] = serde_json::json!([{
        "requested": "missing.sty",
        "scope": "package",
        "surprise": true
    }]);
    assert!(serde_json::from_value::<TraceReport>(trace_report).is_err());

    let mut capabilities: serde_json::Value =
        serde_json::from_str(include_str!("../../tests/golden/v1/capabilities.json")).unwrap();
    capabilities["surprise"] = serde_json::json!(true);
    assert!(serde_json::from_value::<crate::cli::protocol::Capabilities>(capabilities).is_err());

    let mut report: serde_json::Value = serde_json::from_str(include_str!(
        "../../tests/golden/v1/convergence-report.json"
    ))
    .unwrap();
    report["matched"][0]["surprise"] = serde_json::json!(true);
    assert!(serde_json::from_value::<ConvergenceReport>(report).is_err());

    let unresolved = serde_json::json!({
        "requested": "missing.sty",
        "scope": "package",
        "reason": "no-provider"
    });
    assert!(
        serde_json::from_value::<UnresolvedTraceInput>(unresolved.clone()).is_ok(),
        "declared flattened convergence fields must remain readable"
    );
    let mut unknown_unresolved = unresolved;
    unknown_unresolved["surprise"] = serde_json::json!(true);
    assert!(serde_json::from_value::<UnresolvedTraceInput>(unknown_unresolved).is_err());
}

#[test]
fn artifact_deserializers_reject_missing_required_trace_fields() {
    let mut missing_inputs: serde_json::Value =
        serde_json::from_str(include_str!("../../tests/golden/v1/trace.json")).unwrap();
    missing_inputs.as_object_mut().unwrap().remove("inputs");
    assert!(serde_json::from_value::<InputTrace>(missing_inputs).is_err());

    let mut missing_scope: serde_json::Value =
        serde_json::from_str(include_str!("../../tests/golden/v1/trace.json")).unwrap();
    missing_scope["inputs"][0]
        .as_object_mut()
        .unwrap()
        .remove("scope");
    assert!(serde_json::from_value::<InputTrace>(missing_scope).is_err());
}

#[test]
fn lock_stage_invariants_reject_inconsistent_artifacts() {
    let exact: LockFile =
        serde_json::from_str(include_str!("../../tests/golden/v1/lock-exact.json")).unwrap();

    let mut scanned = exact.clone();
    scanned.stage = LockStage::Scanned;
    assert!(
        validate_lock(&scanned)
            .unwrap_err()
            .to_string()
            .contains("scanned")
    );

    let mut resolved = exact.clone();
    resolved.stage = LockStage::Resolved;
    resolved.registries.clear();
    assert!(
        validate_lock(&resolved)
            .unwrap_err()
            .to_string()
            .contains("registry provenance")
    );

    let mut incomplete = exact.clone();
    incomplete.closure[0].files.clear();
    assert!(
        validate_lock(&incomplete)
            .unwrap_err()
            .to_string()
            .contains("empty locked file index")
    );

    let mut untraceable = exact.clone();
    untraceable.closure[0].satisfies.clear();
    assert!(
        validate_lock(&untraceable)
            .unwrap_err()
            .to_string()
            .contains("does not account")
    );

    let mut duplicate_owner = exact.clone();
    let mut other = duplicate_owner.closure[0].clone();
    other.provider = "other".to_string();
    other.direct = true;
    other.satisfies.clear();
    duplicate_owner.closure.push(other);
    assert!(
        validate_lock(&duplicate_owner)
            .unwrap_err()
            .to_string()
            .contains("owned by both")
    );
}
use std::collections::BTreeSet;
use std::fs;
use std::path::Path;

use crate::tests::temporary_test_root;
use crate::{
    CONVERGENCE_REPORT_SCHEMA, ConvergenceReport, ENVIRONMENT_SCHEMA, InputTrace, LOCK_SCHEMA,
    LockFile, LockStage, MemorySourceTree, PROGRESS_SCHEMA, PackageByteSource, ResolvedEnvironment,
    TRACE_REPORT_SCHEMA, TRACE_SCHEMA, TlpdbIndex, TraceReport, UnresolvedTraceInput, VirtualPath,
    hydrate_lock, read_lock, read_trace, resolve, scan_source, stable_convergence_report,
    validate_lock,
};
