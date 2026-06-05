#[test]
fn hydration_adds_static_runtime_providers_and_skips_engine_components() {
    let root = temporary_test_root("runtime-closure");
    let alpha_path = root.join("texmf-dist/tex/latex/alpha/alpha.sty");
    let beta_path = root.join("texmf-dist/tex/latex/beta/beta.sty");
    let gamma_path = root.join("texmf-dist/tex/latex/gamma/gamma.sty");
    fs::create_dir_all(alpha_path.parent().unwrap()).unwrap();
    fs::create_dir_all(beta_path.parent().unwrap()).unwrap();
    fs::create_dir_all(gamma_path.parent().unwrap()).unwrap();
    fs::write(
        &alpha_path,
        br"\RequirePackage{beta}\IfFileExists{gamma.sty}{}{}",
    )
    .unwrap();
    fs::write(&beta_path, br"\ProvidesPackage{beta}").unwrap();
    fs::write(&gamma_path, br"\ProvidesPackage{gamma}").unwrap();

    let sample = concat!(
        "name alpha\n",
        "category Package\n",
        "revision 1\n",
        "depend engine-core\n",
        "runfiles size=1\n",
        " texmf-dist/tex/latex/alpha/alpha.sty\n",
        "\n",
        "name beta\n",
        "category Package\n",
        "revision 1\n",
        "runfiles size=1\n",
        " texmf-dist/tex/latex/beta/beta.sty\n",
        "\n",
        "name gamma\n",
        "category Package\n",
        "revision 1\n",
        "runfiles size=1\n",
        " texmf-dist/tex/latex/gamma/gamma.sty\n",
        "\n",
        "name engine-core\n",
        "category TLCore\n",
        "revision 1\n",
        "runfiles size=1\n",
        " texmf-dist/tex/latex/engine-core/engine-core.sty\n",
    );
    let index = TlpdbIndex::parse(sample, &root.join("tlpkg/texlive.tlpdb"));
    let mut source = MemorySourceTree::default();
    source.insert(
        VirtualPath::new("main.tex").unwrap(),
        br"\usepackage{alpha}",
    );
    let mut lock = scan_source(&source, VirtualPath::new("main.tex").unwrap()).unwrap();
    resolve(&mut lock, &index);
    assert_eq!(
        lock.closure
            .iter()
            .map(|package| package.provider.as_str())
            .collect::<Vec<_>>(),
        vec!["alpha"]
    );

    let store = root.join("store");
    hydrate_lock(
        &mut lock,
        &index,
        &PackageByteSource::local_texlive(&root),
        &store,
    )
    .unwrap();
    let beta = lock
        .closure
        .iter()
        .find(|package| package.provider == "beta")
        .unwrap();
    assert_eq!(beta.runtime_requests, vec!["beta.sty"]);
    assert_eq!(beta.requested_by, vec!["alpha"]);
    let gamma = lock
        .closure
        .iter()
        .find(|package| package.provider == "gamma")
        .unwrap();
    assert_eq!(gamma.runtime_requests, vec!["gamma.sty"]);
    assert_eq!(gamma.requested_by, vec!["alpha"]);
    assert!(
        lock.closure
            .iter()
            .all(|package| package.provider != "engine-core")
    );
    let _ = fs::remove_dir_all(root);
}

#[test]
fn virtual_path_deserialization_preserves_confinement() {
    assert!(serde_json::from_str::<VirtualPath>(r#""project/main.tex""#).is_ok());
    assert!(serde_json::from_str::<VirtualPath>(r#""../outside.tex""#).is_err());
    assert!(serde_json::from_str::<VirtualPath>(r#""/outside.tex""#).is_err());
    assert!(serde_json::from_str::<VirtualPath>(r#""project\\main.tex""#).is_err());
    assert!(serde_json::from_str::<VirtualPath>(r#""C:/project/main.tex""#).is_err());
    assert_eq!(
        VirtualPath::new(r"project\main.tex").unwrap().as_str(),
        "project/main.tex"
    );
}
use std::fs;

use crate::tests::temporary_test_root;
use crate::{
    MemorySourceTree, PackageByteSource, TlpdbIndex, VirtualPath, hydrate_lock, resolve,
    scan_source,
};
