use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

static TEMPORARY_ROOT_SEQUENCE: AtomicU64 = AtomicU64::new(0);

fn temporary_root() -> PathBuf {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock")
        .as_nanos();
    let sequence = TEMPORARY_ROOT_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    std::env::temp_dir().join(format!(
        "pqty-cli-contract-test-{}-{nonce}-{sequence}",
        std::process::id()
    ))
}

fn pqty(root: &Path) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_pqty"));
    command.current_dir(root).arg("--no-config");
    command
}

fn run_ok(command: &mut Command) -> Output {
    let output = command.output().expect("run pqty");
    assert!(
        output.status.success(),
        "command failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    output
}

fn arg_path(command: &mut Command, flag: &str, path: &Path) {
    command.arg(flag).arg(path.as_os_str());
}

#[cfg(unix)]
fn make_writable(path: &Path) {
    use std::os::unix::fs::PermissionsExt as _;
    let metadata = fs::metadata(path).expect("metadata");
    let mut permissions = metadata.permissions();
    permissions.set_mode(permissions.mode() | 0o200);
    fs::set_permissions(path, permissions).expect("writable");
}

#[cfg(windows)]
#[allow(
    clippy::permissions_set_readonly_false,
    reason = "this Windows-only test helper must clear FILE_ATTRIBUTE_READONLY during cleanup"
)]
fn make_writable(path: &Path) {
    let mut permissions = fs::metadata(path).expect("metadata").permissions();
    permissions.set_readonly(false);
    fs::set_permissions(path, permissions).expect("writable");
}

fn make_tree_writable(path: &Path) {
    if !path.exists() {
        return;
    }
    if path.is_dir() {
        for entry in fs::read_dir(path).expect("directory") {
            make_tree_writable(&entry.expect("entry").path());
        }
    } else {
        make_writable(path);
    }
}

#[test]
fn every_supported_pqty_command_has_a_contract_smoke_case() {
    let (root, tlpdb) = create_fixture();
    let (exact_lock, lock_store) = run_resolution_commands(&root, &tlpdb);
    run_environment_commands(&root, &exact_lock, &lock_store);
    make_tree_writable(&root);
    fs::remove_dir_all(root).expect("cleanup");
}

#[test]
fn language_and_lazy_file_contracts_work_offline_across_portable_stores() {
    let (root, tlpdb) = create_language_fixture();
    let capabilities =
        run_ok(pqty(&root).args(["capabilities", "--schema", "pqty.capabilities/v1"]));
    validate_json_schema(
        include_str!("../schemas/pqty.capabilities.schema.json"),
        &capabilities.stdout,
    );
    let mut query = pqty(&root);
    query.args([
        "languages",
        "--language",
        "Demo",
        "--language",
        "demo-alias",
    ]);
    arg_path(&mut query, "--tlpdb", &tlpdb);
    let languages = run_ok(&mut query);
    validate_json_schema(
        include_str!("../schemas/pqty.languages.schema.json"),
        &languages.stdout,
    );
    let languages: pqty::LanguageManifest = serde_json::from_slice(&languages.stdout).unwrap();
    assert_eq!(languages.languages.len(), 1);

    let lock_path = root.join("language.lock");
    let store = root.join("language-store");
    let mut command = pqty(&root);
    command.args([
        "--offline",
        "lock",
        "main.tex",
        "--require-language",
        "Demo",
    ]);
    arg_path(&mut command, "--tlpdb", &tlpdb);
    arg_path(&mut command, "--store", &store);
    arg_path(&mut command, "--output", &lock_path);
    run_ok(&mut command);
    let original = fs::read(&lock_path).unwrap();
    let lock = pqty::read_lock(&lock_path).unwrap();
    assert_eq!(lock.closure.len(), 4);
    assert_eq!(lock.consumer_requirements.files, languages.runtime_files());
    let mut query = pqty(&root);
    query.args(["--offline", "languages", "--language", "demo"]);
    arg_path(&mut query, "--lock", &lock_path);
    let bound = run_ok(&mut query);
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&bound.stdout).unwrap(),
        serde_json::to_value(&languages).unwrap()
    );

    // Synonyms have identical persisted requirements and do not churn locks.
    command.args(["--require-language", "demo-alias"]);
    run_ok(&mut command);
    assert_eq!(fs::read(&lock_path).unwrap(), original);
    let mut files = pqty(&root);
    files.args(["--offline", "files"]);
    arg_path(&mut files, "--lock", &lock_path);
    arg_path(&mut files, "--store", &store);
    let exported = run_ok(&mut files);
    validate_json_schema(
        include_str!("../schemas/pqty.files.schema.json"),
        &exported.stdout,
    );
    let manifest: pqty::FileManifest = serde_json::from_slice(&exported.stdout).unwrap();
    manifest.validate(&lock).unwrap();
    assert_eq!(manifest.files.len(), 5);
    assert!(
        languages
            .runtime_files()
            .iter()
            .all(|path| manifest.files.iter().any(|file| file.tds_path == *path))
    );
    for file in &manifest.files {
        use sha2::{Digest, Sha256};
        let bytes = fs::read(root.join("texmf-dist").join(&file.tds_path)).unwrap();
        assert_eq!(file.size, bytes.len() as u64);
        assert_eq!(
            file.digest,
            format!("sha256:{}", hex::encode(Sha256::digest(&bytes)))
        );
    }

    let (imported_lock, imported_store) = copy_portable_store(&root, &lock_path, &store);
    // Files needs neither registry metadata nor the original package tree.
    fs::remove_file(&tlpdb).unwrap();
    fs::remove_dir_all(root.join("texmf-dist")).unwrap();
    let mut command = pqty(&root);
    command.args(["--offline", "files"]);
    arg_path(&mut command, "--lock", &imported_lock);
    arg_path(&mut command, "--store", &imported_store);
    assert_eq!(run_ok(&mut command).stdout, exported.stdout);
    make_tree_writable(&root);
    fs::remove_dir_all(root).unwrap();
}

fn copy_portable_store(root: &Path, lock_path: &Path, store: &Path) -> (PathBuf, PathBuf) {
    let bundle = root.join("packages.tar.gz");
    let mut command = pqty(root);
    command.arg("bundle");
    arg_path(&mut command, "--lock", lock_path);
    arg_path(&mut command, "--store", store);
    arg_path(&mut command, "--output", &bundle);
    run_ok(&mut command);
    let imported_store = root.join("imported-store");
    let imported_lock = root.join("imported.lock");
    let mut command = pqty(root);
    command.args(["--offline", "import"]);
    arg_path(&mut command, "--bundle", &bundle);
    arg_path(&mut command, "--store", &imported_store);
    arg_path(&mut command, "--lock", &imported_lock);
    run_ok(&mut command);
    (imported_lock, imported_store)
}

#[test]
fn language_extension_and_failed_exports_preserve_existing_artifacts() {
    let (root, tlpdb) = create_language_fixture();
    let lock_path = root.join("pqty.lock");
    let store = root.join("store");
    let mut command = pqty(&root);
    command.args(["--offline", "lock", "main.tex"]);
    arg_path(&mut command, "--tlpdb", &tlpdb);
    arg_path(&mut command, "--store", &store);
    run_ok(&mut command);
    let mut command = pqty(&root);
    command.args(["--offline", "require", "--language", "demo-alias"]);
    arg_path(&mut command, "--store", &store);
    run_ok(&mut command);
    let original = fs::read(&lock_path).unwrap();
    let lock = pqty::read_lock(&lock_path).unwrap();
    assert_eq!(lock.closure.len(), 4);
    let mut command = pqty(&root);
    command.args(["--offline", "require", "--language", "absent"]);
    arg_path(&mut command, "--store", &store);
    assert!(!command.output().unwrap().status.success());
    assert_eq!(fs::read(&lock_path).unwrap(), original);

    let output = root.join("manifest.json");
    fs::write(&output, b"previous artifact").unwrap();
    let mut command = pqty(&root);
    command.args(["--offline", "languages", "--language", "absent"]);
    arg_path(&mut command, "--lock", &lock_path);
    arg_path(&mut command, "--output", &output);
    assert!(!command.output().unwrap().status.success());
    assert_eq!(fs::read(&output).unwrap(), b"previous artifact");
    fs::write(
        &tlpdb,
        fs::read_to_string(&tlpdb)
            .unwrap()
            .replace("revision 1", "revision 99"),
    )
    .unwrap();
    let mut command = pqty(&root);
    command.args(["--offline", "require", "--language", "demo"]);
    arg_path(&mut command, "--store", &store);
    assert!(!command.output().unwrap().status.success());
    assert_eq!(fs::read(&lock_path).unwrap(), original);

    let manifest = pqty::export_file_manifest(&lock, &store).unwrap();
    let digest = manifest.files[0].digest.strip_prefix("sha256:").unwrap();
    let object = store.join(&digest[..2]).join(digest);
    make_writable(&object);
    fs::write(&object, b"corrupt package bytes").unwrap();
    let mut command = pqty(&root);
    command.args(["--offline", "files"]);
    arg_path(&mut command, "--store", &store);
    arg_path(&mut command, "--output", &output);
    let failed = command.output().unwrap();
    assert!(!failed.status.success());
    assert!(String::from_utf8_lossy(&failed.stderr).contains("corrupt"));
    assert_eq!(fs::read(&output).unwrap(), b"previous artifact");
    make_tree_writable(&root);
    fs::remove_dir_all(root).unwrap();
}

fn validate_json_schema(schema: &str, artifact: &[u8]) {
    let schema: serde_json::Value = serde_json::from_str(schema).unwrap();
    jsonschema::draft202012::meta::validate(&schema).unwrap();
    jsonschema::draft202012::validate(
        &schema,
        &serde_json::from_slice::<serde_json::Value>(artifact).unwrap(),
    )
    .unwrap();
}

fn create_language_fixture() -> (PathBuf, PathBuf) {
    use pqty::PackageRegistry as _;
    let (root, tlpdb) = create_fixture();
    let metadata = include_str!("fixtures/languages/texlive.tlpdb")
        .split_once("\n\n")
        .unwrap()
        .1;
    fs::write(
        &tlpdb,
        format!("{}\n{metadata}", fs::read_to_string(&tlpdb).unwrap()),
    )
    .unwrap();
    let index = pqty::TlpdbIndex::load(&tlpdb).unwrap();
    for provider in ["hyphen-demo", "hyphen-data", "hyphen-support"] {
        for file in &index.package(provider).unwrap().runfiles {
            let path = root.join(file);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, b"% synthetic language fixture\n").unwrap();
        }
    }
    (root, tlpdb)
}

fn create_fixture() -> (PathBuf, PathBuf) {
    let root = temporary_root();
    let tlpdb = root.join("tlpkg/texlive.tlpdb");
    fs::create_dir_all(tlpdb.parent().expect("tlpdb parent")).expect("tlpdb directory");
    fs::write(root.join("main.tex"), br"\usepackage{foo}").expect("source");
    for (path, bytes) in [
        ("texmf-dist/tex/latex/foo/foo.sty", b"% foo".as_slice()),
        ("texmf-dist/tex/latex/baz/baz.sty", b"% baz".as_slice()),
    ] {
        let path = root.join(path);
        fs::create_dir_all(path.parent().expect("package parent")).expect("package directory");
        fs::write(path, bytes).expect("package");
    }
    fs::write(
        &tlpdb,
        concat!(
            "name 00texlive.config\n",
            "category TLCore\n",
            "depend release/2026\n",
            "\n",
            "name foo\n",
            "category Package\n",
            "revision 1\n",
            "runfiles size=1\n",
            " texmf-dist/tex/latex/foo/foo.sty\n",
            "\n",
            "name baz\n",
            "category Package\n",
            "revision 2\n",
            "runfiles size=1\n",
            " texmf-dist/tex/latex/baz/baz.sty\n",
        ),
    )
    .expect("tlpdb");
    (root, tlpdb)
}

fn run_resolution_commands(root: &Path, tlpdb: &Path) -> (PathBuf, PathBuf) {
    let capabilities = run_ok(pqty(root).arg("capabilities"));
    let capabilities = serde_json::from_slice::<serde_json::Value>(&capabilities.stdout)
        .expect("capabilities JSON");
    assert_eq!(capabilities["schema"], "pqty.capabilities/v1");
    assert_eq!(capabilities["progress_schema"], "pqty.progress/v1");
    run_ok(pqty(root).args(["scan", "main.tex"]));
    run_ok(pqty(root).args(["explain", "main.tex"]));

    let mut tree = pqty(root);
    tree.args(["tree", "main.tex"]);
    arg_path(&mut tree, "--tlpdb", tlpdb);
    assert!(String::from_utf8_lossy(&run_ok(&mut tree).stdout).contains("foo"));

    let mut why = pqty(root);
    why.args(["why", "main.tex", "foo"]);
    arg_path(&mut why, "--tlpdb", tlpdb);
    assert!(String::from_utf8_lossy(&run_ok(&mut why).stdout).contains("foo"));

    let resolved_lock = root.join("resolved.lock");
    let mut resolve = pqty(root);
    resolve.args(["resolve", "main.tex"]);
    arg_path(&mut resolve, "--tlpdb", tlpdb);
    arg_path(&mut resolve, "--output", &resolved_lock);
    run_ok(&mut resolve);

    let exact_lock = root.join("pqty.lock");
    let lock_store = root.join("lock-store");
    let mut lock = pqty(root);
    lock.args(["lock", "main.tex"]);
    arg_path(&mut lock, "--tlpdb", tlpdb);
    arg_path(&mut lock, "--store", &lock_store);
    arg_path(&mut lock, "--output", &exact_lock);
    run_ok(&mut lock);

    let install_store = root.join("install-store");
    let install_tree = root.join("install-texmf");
    let mut install = pqty(root);
    install.arg("--offline").arg("install");
    arg_path(&mut install, "--lock", &exact_lock);
    arg_path(&mut install, "--store", &install_store);
    arg_path(&mut install, "--out", &install_tree);
    run_ok(&mut install);
    assert!(install_tree.join("tex/latex/foo/foo.sty").is_file());
    (exact_lock, lock_store)
}

fn run_environment_commands(root: &Path, exact_lock: &Path, lock_store: &Path) {
    let mut env = pqty(root);
    env.arg("env");
    arg_path(&mut env, "--lock", exact_lock);
    let environment = serde_json::from_slice::<serde_json::Value>(&run_ok(&mut env).stdout)
        .expect("environment JSON");
    let fingerprint = environment["fingerprint"]
        .as_str()
        .expect("environment fingerprint");

    let trace = root.join("trace.json");
    fs::write(
        &trace,
        serde_json::to_vec_pretty(&serde_json::json!({
            "schema": "pqty.trace/v1",
            "producer": "cli-contract-test",
            "environment_fingerprint": fingerprint,
            "inputs": [{
                "requested": "foo.sty",
                "resolved": "tex/latex/foo/foo.sty",
                "scope": "package",
                "kind": "tex"
            }]
        }))
        .expect("trace JSON"),
    )
    .expect("trace");

    let mut check_trace = pqty(root);
    check_trace.arg("check-trace");
    arg_path(&mut check_trace, "--lock", exact_lock);
    arg_path(&mut check_trace, "--trace", &trace);
    run_ok(&mut check_trace);

    let mut converge = pqty(root);
    converge.arg("converge");
    arg_path(&mut converge, "--lock", exact_lock);
    arg_path(&mut converge, "--trace", &trace);
    let convergence = run_ok(&mut converge);
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&convergence.stdout).expect("convergence JSON")
            ["status"],
        "stable"
    );

    let required_lock = root.join("required.lock");
    let mut require = pqty(root);
    require.args(["require", "--provider", "baz"]);
    arg_path(&mut require, "--lock", exact_lock);
    arg_path(&mut require, "--store", lock_store);
    arg_path(&mut require, "--output", &required_lock);
    run_ok(&mut require);
    assert!(
        fs::read_to_string(required_lock)
            .expect("required lock")
            .contains("\"provider\": \"baz\"")
    );

    let mut invalid_remote = pqty(root);
    invalid_remote.args(["--offline", "resolve", "main.tex", "--remote", "latest"]);
    let output = invalid_remote.output().expect("invalid selector command");
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("cannot be combined"));
}

#[test]
fn direct_usage_errors_do_not_repeat_the_command_name() {
    let root = temporary_root();
    fs::create_dir_all(&root).expect("temporary root");
    let output = pqty(&root)
        .arg("require")
        .output()
        .expect("run invalid require");
    assert!(!output.status.success());
    assert_eq!(
        String::from_utf8_lossy(&output.stderr).trim_end(),
        "pqty: `require` needs at least one --file, --provider, or --language"
    );
    make_tree_writable(&root);
    fs::remove_dir_all(root).expect("remove temporary root");
}

#[test]
fn environment_export_is_atomic_and_matches_stdout() {
    let (root, tlpdb) = create_fixture();
    let (exact_lock, _) = run_resolution_commands(&root, &tlpdb);
    let destination = root.join("environment.json");
    fs::write(&destination, b"previous environment").expect("previous environment");
    let stdout = run_ok(pqty(&root).arg("env").arg("--lock").arg(&exact_lock)).stdout;
    let output = run_ok(
        pqty(&root)
            .arg("env")
            .arg("--lock")
            .arg(&exact_lock)
            .arg("--output")
            .arg(&destination),
    );
    assert!(output.stdout.is_empty(), "{:?}", output.stdout);
    assert_eq!(fs::read(&destination).expect("export"), stdout);

    let invalid_lock = root.join("invalid.lock");
    fs::write(&invalid_lock, b"{}").expect("invalid lock");
    let failure = pqty(&root)
        .arg("env")
        .arg("--lock")
        .arg(&invalid_lock)
        .arg("-o")
        .arg(&destination)
        .output()
        .expect("run pqty");
    assert!(!failure.status.success());
    assert!(failure.stdout.is_empty(), "{:?}", failure.stdout);
    assert_eq!(fs::read(&destination).expect("previous export"), stdout);

    let output_directory = root.join("output-directory");
    fs::create_dir(&output_directory).expect("output directory");
    fs::write(output_directory.join("keep"), b"owned data").expect("existing data");
    let failure = pqty(&root)
        .arg("env")
        .arg("--lock")
        .arg(&exact_lock)
        .arg("-o")
        .arg(&output_directory)
        .output()
        .expect("run pqty");
    assert!(!failure.status.success());
    assert_eq!(
        fs::read(output_directory.join("keep")).expect("preserved data"),
        b"owned data"
    );
    assert!(!fs::read_dir(&root).expect("directory").any(|entry| {
        entry
            .expect("entry")
            .file_name()
            .to_string_lossy()
            .contains(".pqty-write-")
    }));
    make_tree_writable(&root);
    fs::remove_dir_all(root).expect("cleanup");
}

fn lock_local_fixture(root: &Path, tlpdb: &Path) -> Output {
    run_ok(
        pqty(root)
            .args(["--offline", "lock", "main.tex"])
            .arg("--tlpdb")
            .arg(tlpdb)
            .arg("--store")
            .arg(root.join("store"))
            .arg("--output")
            .arg(root.join("pqty.lock")),
    )
}

fn fixture_lock(root: &Path) -> serde_json::Value {
    serde_json::from_slice(&fs::read(root.join("pqty.lock")).unwrap()).unwrap()
}

#[test]
fn local_refresh_repairs_a_legacy_closure_missing_declared_core_data() {
    let (root, tlpdb) = create_fixture();
    let metadata = fs::read_to_string(&tlpdb)
        .unwrap()
        .replace("name foo\n", "name foo\ndepend core-data\n");
    fs::write(&tlpdb, format!("{metadata}\nname core-data\ncategory TLCore\nrevision 1\nrunfiles size=1\n RELOC/web2c/data.tcx\n")).unwrap();
    let data = root.join("texmf-dist/web2c/data.tcx");
    fs::create_dir_all(data.parent().unwrap()).unwrap();
    fs::write(data, b"% core data").unwrap();
    lock_local_fixture(&root, &tlpdb);
    let lock_path = root.join("pqty.lock");
    let mut legacy = pqty::read_lock(&lock_path).unwrap();
    legacy.closure.retain(|entry| entry.provider != "core-data");
    for entry in &mut legacy.closure {
        entry.dependencies.retain(|name| name != "core-data");
    }
    // This remains a valid old exact artifact; refreshing with metadata must
    // check its data dependencies rather than reusing that incomplete graph.
    pqty::write_lock(&lock_path, &legacy).unwrap();
    lock_local_fixture(&root, &tlpdb);
    let repaired = pqty::read_lock(&lock_path).unwrap();
    assert!(
        repaired
            .closure
            .iter()
            .any(|entry| entry.provider == "core-data")
    );
    assert_eq!(
        repaired
            .closure
            .iter()
            .find(|entry| entry.provider == "foo")
            .unwrap()
            .dependencies,
        ["core-data"]
    );
    make_tree_writable(&root);
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn literal_input_changes_and_project_overrides_refresh_the_correct_closure() {
    let (root, tlpdb) = create_fixture();
    for (input, provider) in [("foo.sty", "foo"), ("baz.sty", "baz")] {
        fs::write(root.join("main.tex"), format!("\\input{{{input}}}")).unwrap();
        lock_local_fixture(&root, &tlpdb);
        let lock = pqty::read_lock(&root.join("pqty.lock")).unwrap();
        assert!(lock.unresolved.is_empty());
        assert_eq!(lock.closure.len(), 1);
        assert_eq!(lock.closure[0].provider, provider);
    }
    fs::write(root.join("baz.sty"), b"% project override").unwrap();
    lock_local_fixture(&root, &tlpdb);
    let lock = pqty::read_lock(&root.join("pqty.lock")).unwrap();
    assert!(lock.closure.is_empty());
    assert_eq!(lock.inputs[0].resolved_path.as_deref(), Some("baz.sty"));
    make_tree_writable(&root);
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn local_lock_reuse_checks_source_bytes_metadata_and_requirements() {
    let (root, tlpdb) = create_fixture();
    lock_local_fixture(&root, &tlpdb);
    let first = fixture_lock(&root);
    fs::write(root.join("main.tex"), br"\usepackage{foo} changed prose").unwrap();
    let refreshed = lock_local_fixture(&root, &tlpdb);
    assert!(String::from_utf8_lossy(&refreshed.stdout).contains("refreshed"));
    let second = fixture_lock(&root);
    assert_eq!(first["closure"], second["closure"]);
    assert_ne!(first["sources"], second["sources"]);

    // Same length and timestamp: source-byte validation must still miss.
    let package = root.join("texmf-dist/tex/latex/foo/foo.sty");
    let modified = fs::metadata(&package).unwrap().modified().unwrap();
    fs::write(&package, b"% bar").unwrap();
    fs::OpenOptions::new()
        .write(true)
        .open(&package)
        .unwrap()
        .set_times(fs::FileTimes::new().set_modified(modified))
        .unwrap();
    let changed = lock_local_fixture(&root, &tlpdb);
    assert!(String::from_utf8_lossy(&changed.stdout).contains("wrote"));
    let third = fixture_lock(&root);
    assert_ne!(
        second["closure"][0]["integrity"],
        third["closure"][0]["integrity"]
    );

    let metadata = fs::read_to_string(&tlpdb)
        .unwrap()
        .replace("revision 1", "revision 3");
    fs::write(&tlpdb, metadata).unwrap();
    lock_local_fixture(&root, &tlpdb);
    let fourth = fixture_lock(&root);
    assert_ne!(third["registries"], fourth["registries"]);
    assert_eq!(fourth["closure"][0]["version"], "tlrev:3");

    fs::write(root.join("main.tex"), br"\usepackage{foo,baz}").unwrap();
    lock_local_fixture(&root, &tlpdb);
    assert_eq!(fixture_lock(&root)["closure"].as_array().unwrap().len(), 2);

    fs::remove_file(&package).unwrap();
    let removed = pqty(&root)
        .args(["--offline", "lock", "main.tex"])
        .arg("--tlpdb")
        .arg(&tlpdb)
        .arg("--store")
        .arg(root.join("store"))
        .arg("--output")
        .arg(root.join("pqty.lock"))
        .output()
        .unwrap();
    assert!(
        !removed.status.success(),
        "removed provider must not reuse its old lock"
    );
    make_tree_writable(&root);
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn reused_local_lock_does_not_bypass_store_verification() {
    use sha2::{Digest as _, Sha256};
    let (root, tlpdb) = create_fixture();
    lock_local_fixture(&root, &tlpdb);
    let digest = hex::encode(Sha256::digest(b"% foo"));
    let object = root.join("store").join(&digest[..2]).join(&digest);
    make_writable(&object);
    fs::write(&object, b"% bad").unwrap();
    let reused = lock_local_fixture(&root, &tlpdb);
    assert!(String::from_utf8_lossy(&reused.stdout).contains("current"));
    let install = pqty(&root)
        .args(["--offline", "install", "--link", "copy"])
        .arg("--lock")
        .arg(root.join("pqty.lock"))
        .arg("--store")
        .arg(root.join("store"))
        .arg("--out")
        .arg(root.join("out"))
        .output()
        .unwrap();
    assert!(!install.status.success());
    assert!(String::from_utf8_lossy(&install.stderr).contains("corrupt"));
    assert!(!root.join("out/tex/latex/foo/foo.sty").exists());
    make_tree_writable(&root);
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn typed_maps_and_single_file_exports_have_negotiated_offline_cli_contracts() {
    let root = temporary_root();
    fs::create_dir_all(root.join("tlpkg")).unwrap();
    let tlpdb = root.join("tlpkg/texlive.tlpdb");
    let metadata = include_str!("fixtures/font-maps/texlive.tlpdb");
    fs::write(&tlpdb, metadata).unwrap();
    let index = pqty::TlpdbIndex::load(&tlpdb).unwrap();
    for provider in ["demo", "concrete"] {
        for file in &pqty::PackageRegistry::package(&index, provider)
            .unwrap()
            .runfiles
        {
            let path = root
                .join("texmf-dist")
                .join(file.strip_prefix("RELOC/").unwrap());
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, b"% fixture\n").unwrap();
        }
    }
    fs::write(root.join("main.tex"), br"\usepackage{demo}").unwrap();
    let capabilities =
        run_ok(pqty(&root).args(["capabilities", "--schema", "pqty.capabilities/v1"]));
    validate_json_schema(
        include_str!("../schemas/pqty.capabilities.schema.json"),
        &capabilities.stdout,
    );
    let mut lock = pqty(&root);
    lock.args(["--offline", "lock", "main.tex", "--store", "store"]);
    arg_path(&mut lock, "--tlpdb", &tlpdb);
    run_ok(&mut lock);
    let mut maps = pqty(&root);
    maps.args(["--offline", "font-maps", "--output", "maps.json"]);
    run_ok(&mut maps);
    let original = fs::read(root.join("maps.json")).unwrap();
    validate_json_schema(
        include_str!("../schemas/pqty.font-maps.schema.json"),
        &original,
    );
    let parsed: pqty::FontMapManifest = serde_json::from_slice(&original).unwrap();
    assert_eq!(parsed.maps.len(), 4);
    let mut file = pqty(&root);
    file.args([
        "--offline",
        "file",
        "--store",
        "store",
        "--path",
        "fonts/map/dvipdfmx/demo/morisawa5.map",
        "--output",
        "export.map",
    ]);
    let output = run_ok(&mut file);
    assert!(output.stdout.is_empty(), "{:?}", output.stdout);
    assert_eq!(fs::read(root.join("export.map")).unwrap(), b"% fixture\n");
    // File delivery needs neither upstream metadata nor the original source tree.
    fs::write(&tlpdb, metadata.replace("revision 1", "revision 9")).unwrap();
    let failure = maps.output().unwrap();
    assert!(!failure.status.success());
    assert_eq!(fs::read(root.join("maps.json")).unwrap(), original);
    fs::remove_dir_all(root.join("texmf-dist")).unwrap();
    fs::remove_file(&tlpdb).unwrap();
    run_ok(&mut file);
    file.args(["--path", "absent.map"]);
    assert!(!file.output().unwrap().status.success());
    assert_eq!(fs::read(root.join("export.map")).unwrap(), b"% fixture\n");
    make_tree_writable(&root);
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn standalone_package_commands_need_no_project_and_export_only_one_file() {
    let (root, tlpdb) = create_fixture();
    fs::remove_file(root.join("main.tex")).unwrap();
    let mut command = pqty(&root);
    command.args([
        "--offline",
        "package",
        "--file",
        "foo.sty",
        "--store",
        "store",
        "--output",
        "foo.package.json",
    ]);
    arg_path(&mut command, "--tlpdb", &tlpdb);
    run_ok(&mut command);
    let artifact = fs::read(root.join("foo.package.json")).unwrap();
    validate_json_schema(
        include_str!("../schemas/pqty.package.schema.json"),
        &artifact,
    );
    let package: pqty::PackageManifest = serde_json::from_slice(&artifact).unwrap();
    assert_eq!(package.provider, "foo");
    assert_eq!(package.files.len(), 1);
    assert!(!root.join("main.tex").exists());
    assert!(!root.join("pqty.lock").exists());
    let output = run_ok(pqty(&root).args([
        "--offline",
        "package-file",
        "--manifest",
        "foo.package.json",
        "--store",
        "store",
        "--path",
        "tex/latex/foo/foo.sty",
        "--output",
        "public.sty",
    ]));
    assert!(output.stdout.is_empty(), "{:?}", output.stdout);
    assert_eq!(fs::read(root.join("public.sty")).unwrap(), b"% foo");
    for version in ["v2", "v3"] {
        let result = pqty(&root)
            .args([
                "capabilities",
                "--schema",
                &format!("pqty.capabilities/{version}"),
            ])
            .output()
            .unwrap();
        assert!(!result.status.success());
    }
    make_tree_writable(&root);
    fs::remove_dir_all(root).unwrap();
}
