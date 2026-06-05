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
        "pqty: `require` needs at least one --file or --provider"
    );
    make_tree_writable(&root);
    fs::remove_dir_all(root).expect("remove temporary root");
}
