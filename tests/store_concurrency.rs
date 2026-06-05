use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{SystemTime, UNIX_EPOCH};

fn temporary_root() -> PathBuf {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock")
        .as_nanos();
    std::env::temp_dir().join(format!(
        "pqty-multiprocess-store-test-{}-{nonce}",
        std::process::id()
    ))
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
        for entry in fs::read_dir(path).expect("read directory") {
            make_tree_writable(&entry.expect("entry").path());
        }
    } else {
        make_writable(path);
    }
}

#[test]
fn concurrent_cli_processes_share_one_store_without_overwrite() {
    let root = temporary_root();
    let tlpdb = root.join("tlpkg/texlive.tlpdb");
    let package = root.join("texmf-dist/tex/latex/foo/foo.sty");
    fs::create_dir_all(tlpdb.parent().expect("tlpdb parent")).expect("tlpdb directory");
    fs::create_dir_all(package.parent().expect("package parent")).expect("package directory");
    fs::write(root.join("main.tex"), br"\usepackage{foo}").expect("source");
    fs::write(&package, br"\ProvidesPackage{foo}").expect("package");
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
        ),
    )
    .expect("tlpdb");

    let store = root.join("store");
    let mut children = Vec::new();
    for worker in 0..6 {
        let output = root.join(format!("worker-{worker}.lock"));
        let mut command = Command::new(env!("CARGO_BIN_EXE_pqty"));
        command
            .current_dir(&root)
            .arg("--no-config")
            .arg("lock")
            .arg("main.tex")
            .arg("--tlpdb")
            .arg(&tlpdb)
            .arg("--store")
            .arg(&store)
            .arg("--output")
            .arg(output)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        children.push(command.spawn().expect("spawn pqty"));
    }

    for child in children {
        let output = child.wait_with_output().expect("wait for pqty");
        assert!(
            output.status.success(),
            "pqty failed\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }

    let expected = fs::read(root.join("worker-0.lock")).expect("first lock");
    for worker in 1..6 {
        assert_eq!(
            fs::read(root.join(format!("worker-{worker}.lock"))).expect("worker lock"),
            expected
        );
    }
    let objects = fs::read_dir(&store)
        .expect("store")
        .filter_map(Result::ok)
        .filter(|entry| {
            let name = entry.file_name();
            let name = name.to_string_lossy();
            name.len() == 2 && name.bytes().all(|byte| byte.is_ascii_hexdigit())
        })
        .flat_map(|entry| fs::read_dir(entry.path()).expect("object shard"))
        .filter_map(Result::ok)
        .collect::<Vec<_>>();
    assert!(!objects.is_empty());
    assert!(objects.iter().all(|entry| {
        entry
            .metadata()
            .expect("object metadata")
            .permissions()
            .readonly()
    }));

    make_tree_writable(&root);
    fs::remove_dir_all(root).expect("cleanup");
}
