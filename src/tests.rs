use std::fs;
use std::path::{Path, PathBuf};

use crate::{
    LockFile, MemorySourceTree, PackageByteSource, TlpdbIndex, VirtualPath, hydrate_lock, resolve,
    scan_source,
};

pub(crate) fn temporary_test_root(label: &str) -> PathBuf {
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    std::env::temp_dir().join(format!("pqty-{label}-test-{}-{nonce}", std::process::id()))
}

#[cfg(unix)]
fn make_test_writable(path: &Path) {
    use std::os::unix::fs::PermissionsExt as _;
    let metadata = fs::metadata(path).unwrap();
    let mut permissions = metadata.permissions();
    permissions.set_mode(permissions.mode() | 0o200);
    fs::set_permissions(path, permissions).unwrap();
}

#[cfg(windows)]
#[allow(
    clippy::permissions_set_readonly_false,
    reason = "this Windows-only test helper must clear FILE_ATTRIBUTE_READONLY during cleanup"
)]
fn make_test_writable(path: &Path) {
    let mut permissions = fs::metadata(path).unwrap().permissions();
    permissions.set_readonly(false);
    fs::set_permissions(path, permissions).unwrap();
}

fn make_test_tree_writable(path: &Path) {
    if !path.exists() {
        return;
    }
    if path.is_dir() {
        for entry in fs::read_dir(path).unwrap() {
            make_test_tree_writable(&entry.unwrap().path());
        }
    } else {
        make_test_writable(path);
    }
}

fn convergence_fixture() -> (PathBuf, TlpdbIndex, LockFile) {
    let root = temporary_test_root("convergence");
    let tlpdb = root.join("tlpkg/texlive.tlpdb");
    fs::create_dir_all(tlpdb.parent().unwrap()).unwrap();
    let metadata = concat!(
        "name 00texlive.config\n",
        "category TLCore\n",
        "depend release/2026\n",
        "\n",
        "name alpha\n",
        "category Package\n",
        "revision 1\n",
        "runfiles size=1\n",
        " texmf-dist/tex/latex/alpha/alpha.sty\n",
        "\n",
        "name beta\n",
        "category Package\n",
        "revision 2\n",
        "depend gamma\n",
        "depend engine-core\n",
        "runfiles size=1\n",
        " texmf-dist/tex/latex/beta/beta.sty\n",
        "\n",
        "name gamma\n",
        "category Package\n",
        "revision 3\n",
        "runfiles size=1\n",
        " texmf-dist/tex/latex/gamma/gamma.sty\n",
        "\n",
        "name tex-gyre\n",
        "category Package\n",
        "revision 3\n",
        "runfiles size=1\n",
        " texmf-dist/fonts/opentype/public/tex-gyre/texgyreheros-regular.otf\n",
        "\n",
        "name lato\n",
        "category Package\n",
        "revision 3\n",
        "runfiles size=1\n",
        " texmf-dist/fonts/truetype/typoland/lato/Lato-Regular.ttf\n",
        "\n",
        "name engine-core\n",
        "category TLCore\n",
        "revision 4\n",
        "runfiles size=1\n",
        " texmf-dist/tex/generic/engine/engine.tex\n",
    );
    fs::write(&tlpdb, metadata).unwrap();
    for (path, bytes) in [
        (
            "texmf-dist/tex/latex/alpha/alpha.sty",
            b"% alpha".as_slice(),
        ),
        ("texmf-dist/tex/latex/beta/beta.sty", b"% beta".as_slice()),
        (
            "texmf-dist/tex/latex/gamma/gamma.sty",
            b"% gamma".as_slice(),
        ),
        (
            "texmf-dist/fonts/opentype/public/tex-gyre/texgyreheros-regular.otf",
            b"test font".as_slice(),
        ),
        (
            "texmf-dist/fonts/truetype/typoland/lato/Lato-Regular.ttf",
            b"test true type font".as_slice(),
        ),
        (
            "texmf-dist/tex/generic/engine/engine.tex",
            b"% engine".as_slice(),
        ),
    ] {
        let path = root.join(path);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, bytes).unwrap();
    }

    let mut index = TlpdbIndex::load(&tlpdb).unwrap();
    index.retain_installed_runfiles();
    let mut source = MemorySourceTree::default();
    source.insert(
        VirtualPath::new("main.tex").unwrap(),
        br"\usepackage{alpha}",
    );
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

mod environment;
mod hydration;
mod protocol;
mod registry;
mod scanner;
mod source;
mod store;
