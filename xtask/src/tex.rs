use std::collections::BTreeSet;
use std::ffi::{OsStr, OsString};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use pqty::{ConvergenceReport, ConvergenceStatus, LockFile, TraceReport};
use serde::Serialize;

use crate::command::{
    Binaries, ScratchDir, capture, directory_bytes, nonempty_file, read_json, repo_root,
    require_tools, run, run_to_file, run_to_files_allow_failure, search_path, write_json,
};
use crate::{Result, message};

const CORPUS_CASES: &[CorpusCase] = &[
    CorpusCase::static_case("pdflatex-bibtex", "pdflatex", Bibliography::Bibtex),
    CorpusCase::static_case("lualatex-fonts", "lualatex", Bibliography::None),
    CorpusCase::static_case("xelatex-tikz", "xelatex", Bibliography::None),
    CorpusCase::static_case("pdflatex-biber", "pdflatex", Bibliography::Biber),
    CorpusCase::runtime("runtime-local"),
];

pub(crate) fn verify_all() -> Result<()> {
    let harness = Harness::new()?;
    verify_common_with(&harness)?;
    verify_convergence_with(&harness)?;
    verify_corpus_with(&harness, &[])?;
    Ok(())
}

pub(crate) fn verify_common() -> Result<()> {
    verify_common_with(&Harness::new()?)
}

pub(crate) fn verify_convergence() -> Result<()> {
    verify_convergence_with(&Harness::new()?)
}

pub(crate) fn verify_corpus(selected: &[String]) -> Result<()> {
    verify_corpus_with(&Harness::new()?, selected)
}

struct Harness {
    repo: PathBuf,
    binaries: Binaries,
}

impl Harness {
    fn new() -> Result<Self> {
        let repo = repo_root()?;
        let binaries = Binaries::build(&repo)?;
        Ok(Self { repo, binaries })
    }

    fn pqty(&self) -> Command {
        Command::new(&self.binaries.pqty)
    }

    fn pqty_fls(&self) -> Command {
        Command::new(&self.binaries.pqty_fls)
    }

    fn lock(&self, root: &Path, store: &Path, output: &Path, tlpdb: Option<&Path>) -> Result<()> {
        let mut command = self.pqty();
        command
            .arg("--no-config")
            .arg("lock")
            .arg(root)
            .arg("--store")
            .arg(store)
            .arg("--output")
            .arg(output);
        if let Some(tlpdb) = tlpdb {
            command.arg("--tlpdb").arg(tlpdb);
        } else if let Some(remote) = std::env::var_os("PQTY_XTASK_REMOTE") {
            command.arg("--remote").arg(remote);
        }
        run(&mut command)
    }

    fn install(&self, lock: &Path, store: &Path, output: &Path) -> Result<()> {
        let mut command = self.pqty();
        command
            .arg("--no-config")
            .arg("install")
            .arg("--lock")
            .arg(lock)
            .arg("--store")
            .arg(store)
            .arg("--out")
            .arg(output)
            .args(["--link", "copy"]);
        run(&mut command)
    }

    fn environment(&self, lock: &Path, output: &Path) -> Result<()> {
        let mut command = self.pqty();
        command
            .arg("--no-config")
            .arg("env")
            .arg("--lock")
            .arg(lock);
        run_to_file(&mut command, output)
    }

    fn converge(&self, lock: &Path, trace: &Path, store: &Path, output: &Path) -> Result<()> {
        let mut command = self.pqty();
        command
            .arg("--no-config")
            .arg("converge")
            .arg("--lock")
            .arg(lock)
            .arg("--trace")
            .arg(trace)
            .arg("--store")
            .arg(store);
        run_to_file(&mut command, output)
    }

    fn check_trace(&self, lock: &Path, trace: &Path, output: &Path) -> Result<()> {
        let mut command = self.pqty();
        command
            .arg("--no-config")
            .arg("check-trace")
            .arg("--lock")
            .arg(lock)
            .arg("--trace")
            .arg(trace);
        run_to_file(&mut command, output)
    }

    fn check_trace_allow_failure(
        &self,
        lock: &Path,
        trace: &Path,
        output: &Path,
        error: &Path,
    ) -> Result<()> {
        let mut command = self.pqty();
        command
            .arg("--no-config")
            .arg("check-trace")
            .arg("--lock")
            .arg(lock)
            .arg("--trace")
            .arg(trace);
        run_to_files_allow_failure(&mut command, output, error)?;
        Ok(())
    }

    fn adapt_trace(&self, request: &AdaptTrace<'_>) -> Result<()> {
        let mut command = self.pqty_fls();
        command
            .arg("--fls")
            .arg(request.fls)
            .arg("--environment")
            .arg(request.environment)
            .arg("--project-root")
            .arg(request.project)
            .arg("--output-root")
            .arg(request.build);
        for root in request.package_roots {
            command.arg("--package-root").arg(root);
        }
        for root in request.engine_roots {
            command.arg("--engine-root").arg(root);
        }
        command.arg("--output").arg(request.output);
        run(&mut command)
    }
}

struct AdaptTrace<'a> {
    fls: &'a Path,
    environment: &'a Path,
    project: &'a Path,
    build: &'a Path,
    package_roots: &'a [PathBuf],
    engine_roots: &'a [PathBuf],
    output: &'a Path,
}

fn verify_common_with(harness: &Harness) -> Result<()> {
    println!("xtask: verifying common TeX project");
    require_tools(&["pdflatex", "bibtex", "kpsewhich"])?;
    let scratch = ScratchDir::new("common")?;
    let work = scratch.path();
    let fixture = harness.repo.join("examples/common");
    let lock = work.join("pqty.lock");
    let texmf = work.join("texmf");
    let build = work.join("build");

    harness.lock(
        &fixture.join("main.tex"),
        &work.join("lock-store"),
        &lock,
        None,
    )?;
    harness.install(&lock, &work.join("install-store"), &texmf)?;
    fs::create_dir(&build)?;

    run_tex_engine("pdflatex", &fixture, &build, &texmf, None)?;
    let mut bibtex = Command::new("bibtex");
    bibtex
        .current_dir(&build)
        .env(
            "BIBINPUTS",
            search_path([
                OsString::from("."),
                fixture.as_os_str().to_owned(),
                OsString::new(),
            ])?,
        )
        .arg("main");
    run(&mut bibtex)?;
    run_tex_engine("pdflatex", &fixture, &build, &texmf, None)?;
    run_tex_engine("pdflatex", &fixture, &build, &texmf, None)?;

    nonempty_file(&build.join("main.pdf"))?;
    let mut kpsewhich = Command::new("kpsewhich");
    kpsewhich.env("TEXMFHOME", &texmf).arg("plain.bst");
    let plain_bst = PathBuf::from(capture(&mut kpsewhich)?.trim());
    let expected_bst = texmf.join("bibtex/bst/base/plain.bst");
    if plain_bst != expected_bst {
        return Err(message(format!(
            "plain.bst resolved outside the installed tree:\n  expected: {}\n  actual:   {}",
            expected_bst.display(),
            plain_bst.display()
        )));
    }
    verify_input_provenance(&build.join("main.fls"), &texmf, &fixture, &build)?;
    println!("xtask: common lock, clean install, bibliography, PDF, and provenance passed");
    Ok(())
}

fn verify_convergence_with(harness: &Harness) -> Result<()> {
    println!("xtask: verifying runtime convergence");
    require_tools(&["pdflatex", "kpsewhich"])?;
    let scratch = ScratchDir::new("convergence")?;
    let work = scratch.path();
    let fixture = harness.repo.join("examples/convergence");
    let lock = work.join("pqty.lock");
    let store = work.join("store");
    let texmf = work.join("texmf");
    let texlive = TexLive::discover()?;
    let engine_roots = texlive.convergence_engine_roots();

    harness.lock(&fixture.join("main.tex"), &store, &lock, None)?;
    if lock_has_provider(&lock, "xcolor")? {
        return Err(message(
            "static convergence lock unexpectedly contains xcolor",
        ));
    }
    harness.install(&lock, &store, &texmf)?;

    let discovery_environment = work.join("discovery.env.json");
    let discovery_build = work.join("build-discovery");
    let discovery_trace = work.join("discovery.trace.json");
    harness.environment(&lock, &discovery_environment)?;
    fs::create_dir(&discovery_build)?;
    run_tex_with_inputs(
        "pdflatex",
        &fixture,
        &discovery_build,
        &search_path([
            OsString::from("."),
            recursive_path(&texmf),
            recursive_path(&texlive.dist),
        ])?,
    )?;
    harness.adapt_trace(&AdaptTrace {
        fls: &discovery_build.join("main.fls"),
        environment: &discovery_environment,
        project: &fixture,
        build: &discovery_build,
        package_roots: &[texmf.clone(), texlive.dist.clone()],
        engine_roots: &engine_roots,
        output: &discovery_trace,
    })?;

    let changed_path = work.join("changed.report.json");
    harness.converge(&lock, &discovery_trace, &store, &changed_path)?;
    require_convergence_status(&changed_path, ConvergenceStatus::Changed)?;
    if !lock_has_provider(&lock, "xcolor")? {
        return Err(message("converged lock does not contain xcolor"));
    }

    harness.install(&lock, &store, &texmf)?;
    let frozen_environment = work.join("frozen.env.json");
    let frozen_build = work.join("build-frozen");
    let frozen_trace = work.join("frozen.trace.json");
    harness.environment(&lock, &frozen_environment)?;
    fs::create_dir(&frozen_build)?;
    run_tex_with_inputs(
        "pdflatex",
        &fixture,
        &frozen_build,
        &search_path([
            OsString::from("."),
            recursive_path(&texmf),
            recursive_path(&texlive.dist.join("tex/latex/l3backend")),
        ])?,
    )?;
    harness.adapt_trace(&AdaptTrace {
        fls: &frozen_build.join("main.fls"),
        environment: &frozen_environment,
        project: &fixture,
        build: &frozen_build,
        package_roots: std::slice::from_ref(&texmf),
        engine_roots: &engine_roots,
        output: &frozen_trace,
    })?;
    let stable_path = work.join("stable.report.json");
    harness.converge(&lock, &frozen_trace, &store, &stable_path)?;
    require_convergence_status(&stable_path, ConvergenceStatus::Stable)?;
    nonempty_file(&frozen_build.join("main.pdf"))?;
    println!("xtask: dynamic discovery, changed lock, frozen build, and stable trace passed");
    Ok(())
}

fn verify_corpus_with(harness: &Harness, requested: &[String]) -> Result<()> {
    println!("xtask: verifying TeX acceptance corpus");
    let selected = selected_cases(requested)?;
    let mut tools = BTreeSet::from(["kpsewhich"]);
    for case in &selected {
        tools.insert(case.engine);
        match case.bibliography {
            Bibliography::Bibtex => {
                tools.insert("bibtex");
            }
            Bibliography::Biber => {
                tools.insert("biber");
            }
            Bibliography::None => {}
        }
    }
    require_tools(&tools.into_iter().collect::<Vec<_>>())?;

    let scratch = ScratchDir::new("corpus")?;
    let work = scratch.path();
    let texlive = TexLive::discover()?;
    let extra_engine_root = optional_existing_directory("PQTY_CORPUS_ENGINE_ROOT")?;
    let tlpdb = optional_existing_file("PQTY_CORPUS_TLPDB")?;
    let engine_roots = texlive.corpus_engine_roots(extra_engine_root.as_deref());
    let mut metrics = Vec::new();

    for case in selected {
        let metric = if case.runtime {
            run_runtime_case(
                harness,
                case.name,
                work,
                &texlive,
                &engine_roots,
                extra_engine_root.as_deref(),
                tlpdb.as_deref(),
            )?
        } else {
            run_static_case(
                harness,
                case,
                work,
                &engine_roots,
                extra_engine_root.as_deref(),
                tlpdb.as_deref(),
            )?
        };
        metrics.push(metric);
    }

    let metrics_path = harness.repo.join("target/corpus-metrics.json");
    if let Some(parent) = metrics_path.parent() {
        fs::create_dir_all(parent)?;
    }
    write_json(&metrics_path, &metrics)?;
    println!(
        "xtask: {} corpus project(s) passed; metrics: {}",
        metrics.len(),
        metrics_path.display()
    );
    Ok(())
}

fn run_static_case(
    harness: &Harness,
    case: CorpusCase,
    work: &Path,
    engine_roots: &[PathBuf],
    extra_engine_root: Option<&Path>,
    tlpdb: Option<&Path>,
) -> Result<CorpusMetric> {
    println!("xtask: corpus case {}", case.name);
    let project = harness.repo.join("corpus").join(case.name);
    let case_work = work.join(case.name);
    let build = case_work.join("build");
    let store = case_work.join("store");
    let lock = case_work.join("pqty.lock");
    let texmf = case_work.join("texmf");
    fs::create_dir_all(&build)?;

    harness.lock(&project.join("main.tex"), &store, &lock, tlpdb)?;
    harness.install(&lock, &store, &texmf)?;
    run_tex_engine(case.engine, &project, &build, &texmf, extra_engine_root)?;
    match case.bibliography {
        Bibliography::Bibtex => {
            let mut command = Command::new("bibtex");
            command
                .current_dir(&build)
                .env(
                    "BIBINPUTS",
                    search_path([
                        OsString::from("."),
                        project.as_os_str().to_owned(),
                        OsString::new(),
                    ])?,
                )
                .arg("main");
            run(&mut command)?;
        }
        Bibliography::Biber => {
            let mut command = Command::new("biber");
            command
                .current_dir(&project)
                .arg("--input-directory")
                .arg(&build)
                .arg("--output-directory")
                .arg(&build)
                .arg("main");
            run(&mut command)?;
        }
        Bibliography::None => {}
    }
    if case.bibliography != Bibliography::None {
        run_tex_engine(case.engine, &project, &build, &texmf, extra_engine_root)?;
        run_tex_engine(case.engine, &project, &build, &texmf, extra_engine_root)?;
    }
    nonempty_file(&build.join("main.pdf"))?;
    let misses = adapt_frozen_trace(harness, &project, &case_work, engine_roots)?;
    corpus_metric(case.name, &lock, &store, misses, 0)
}

fn run_runtime_case(
    harness: &Harness,
    name: &str,
    work: &Path,
    texlive: &TexLive,
    engine_roots: &[PathBuf],
    extra_engine_root: Option<&Path>,
    tlpdb: Option<&Path>,
) -> Result<CorpusMetric> {
    println!("xtask: corpus case {name}");
    let project = harness.repo.join("corpus").join(name);
    let case_work = work.join(name);
    let discovery_build = case_work.join("build-discovery");
    let build = case_work.join("build");
    let store = case_work.join("store");
    let lock = case_work.join("pqty.lock");
    let texmf = case_work.join("texmf");
    fs::create_dir_all(&discovery_build)?;
    fs::create_dir_all(&build)?;

    harness.lock(&project.join("main.tex"), &store, &lock, tlpdb)?;
    harness.install(&lock, &store, &texmf)?;
    let discovery_environment = case_work.join("discovery-environment.json");
    let discovery_trace = case_work.join("discovery-trace.json");
    harness.environment(&lock, &discovery_environment)?;
    run_tex_with_inputs(
        "pdflatex",
        &project,
        &discovery_build,
        &search_path([
            OsString::from("."),
            recursive_path(&texmf),
            recursive_path(&texlive.dist),
        ])?,
    )?;
    harness.adapt_trace(&AdaptTrace {
        fls: &discovery_build.join("main.fls"),
        environment: &discovery_environment,
        project: &project,
        build: &discovery_build,
        package_roots: &[texmf.clone(), texlive.dist.clone()],
        engine_roots,
        output: &discovery_trace,
    })?;
    let discovery_report = case_work.join("discovery-trace-report.json");
    harness.check_trace_allow_failure(
        &lock,
        &discovery_trace,
        &discovery_report,
        &case_work.join("discovery-trace-report.stderr"),
    )?;
    let misses = read_json::<TraceReport>(&discovery_report)?.missing.len();

    let changed_report = case_work.join("changed-report.json");
    harness.converge(&lock, &discovery_trace, &store, &changed_report)?;
    require_convergence_status(&changed_report, ConvergenceStatus::Changed)?;
    harness.install(&lock, &store, &texmf)?;
    run_tex_engine("pdflatex", &project, &build, &texmf, extra_engine_root)?;
    adapt_frozen_trace(harness, &project, &case_work, engine_roots)?;
    let stable_report = case_work.join("stable-report.json");
    harness.converge(&lock, &case_work.join("trace.json"), &store, &stable_report)?;
    require_convergence_status(&stable_report, ConvergenceStatus::Stable)?;
    corpus_metric(name, &lock, &store, misses, 2)
}

fn adapt_frozen_trace(
    harness: &Harness,
    project: &Path,
    case_work: &Path,
    engine_roots: &[PathBuf],
) -> Result<usize> {
    let lock = case_work.join("pqty.lock");
    let environment = case_work.join("environment.json");
    let trace = case_work.join("trace.json");
    let report = case_work.join("trace-report.json");
    let build = case_work.join("build");
    let texmf = case_work.join("texmf");
    harness.environment(&lock, &environment)?;
    harness.adapt_trace(&AdaptTrace {
        fls: &build.join("main.fls"),
        environment: &environment,
        project,
        build: &build,
        package_roots: std::slice::from_ref(&texmf),
        engine_roots,
        output: &trace,
    })?;
    harness.check_trace(&lock, &trace, &report)?;
    Ok(read_json::<TraceReport>(&report)?.missing.len())
}

fn run_tex_engine(
    engine: &str,
    project: &Path,
    build: &Path,
    texmf: &Path,
    extra_engine_root: Option<&Path>,
) -> Result<()> {
    let texmf_home = if let Some(extra) = extra_engine_root {
        search_path([texmf.as_os_str(), extra.as_os_str()])?
    } else {
        texmf.as_os_str().to_owned()
    };
    let mut command = Command::new(engine);
    command
        .current_dir(project)
        .env("TEXMFHOME", texmf_home)
        .args(["-interaction=nonstopmode", "-halt-on-error", "-recorder"])
        .arg("-output-directory")
        .arg(build)
        .arg("main.tex");
    run(&mut command)
}

fn run_tex_with_inputs(
    engine: &str,
    project: &Path,
    build: &Path,
    texinputs: &OsStr,
) -> Result<()> {
    let mut command = Command::new(engine);
    command
        .current_dir(project)
        .env("TEXINPUTS", texinputs)
        .args(["-interaction=nonstopmode", "-halt-on-error", "-recorder"])
        .arg("-output-directory")
        .arg(build)
        .arg("main.tex");
    run(&mut command)
}

fn verify_input_provenance(
    recorder: &Path,
    texmf: &Path,
    fixture: &Path,
    build: &Path,
) -> Result<()> {
    let text = fs::read_to_string(recorder)?;
    const PACKAGE_EXTENSIONS: &[&str] = &["cls", "sty", "def", "cfg", "clo", "ltx", "mkii"];
    for line in text.lines() {
        let Some(input) = line.strip_prefix("INPUT ") else {
            continue;
        };
        let path = Path::new(input);
        let extension = path.extension().and_then(OsStr::to_str).unwrap_or_default();
        if !PACKAGE_EXTENSIONS.contains(&extension) || !path.is_absolute() {
            continue;
        }
        let engine_backend = extension == "def"
            && path
                .components()
                .any(|component| component.as_os_str() == "l3backend");
        if !path.starts_with(texmf)
            && !path.starts_with(fixture)
            && !path.starts_with(build)
            && !engine_backend
        {
            return Err(message(format!(
                "package input escaped the pqty tree: {}",
                path.display()
            )));
        }
    }
    Ok(())
}

fn require_convergence_status(path: &Path, expected: ConvergenceStatus) -> Result<()> {
    let report: ConvergenceReport = read_json(path)?;
    if report.status == expected {
        Ok(())
    } else {
        Err(message(format!(
            "{} has convergence status {:?}; expected {expected:?}",
            path.display(),
            report.status
        )))
    }
}

fn lock_has_provider(path: &Path, provider: &str) -> Result<bool> {
    let lock: LockFile = read_json(path)?;
    Ok(lock
        .closure
        .iter()
        .any(|package| package.provider == provider))
}

fn recursive_path(path: &Path) -> OsString {
    let mut value = path.as_os_str().to_owned();
    value.push("//");
    value
}

fn optional_existing_directory(name: &str) -> Result<Option<PathBuf>> {
    optional_existing_path(name, Path::is_dir, "directory")
}

fn optional_existing_file(name: &str) -> Result<Option<PathBuf>> {
    optional_existing_path(name, Path::is_file, "file")
}

fn optional_existing_path(
    name: &str,
    predicate: impl Fn(&Path) -> bool,
    kind: &str,
) -> Result<Option<PathBuf>> {
    let Some(value) = std::env::var_os(name) else {
        return Ok(None);
    };
    if value.is_empty() {
        return Ok(None);
    }
    let path = PathBuf::from(value);
    if !predicate(&path) {
        return Err(message(format!(
            "{name} is not an existing {kind}: {}",
            path.display()
        )));
    }
    Ok(Some(path))
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Bibliography {
    None,
    Bibtex,
    Biber,
}

#[derive(Clone, Copy)]
struct CorpusCase {
    name: &'static str,
    engine: &'static str,
    bibliography: Bibliography,
    runtime: bool,
}

impl CorpusCase {
    const fn static_case(
        name: &'static str,
        engine: &'static str,
        bibliography: Bibliography,
    ) -> Self {
        Self {
            name,
            engine,
            bibliography,
            runtime: false,
        }
    }

    const fn runtime(name: &'static str) -> Self {
        Self {
            name,
            engine: "pdflatex",
            bibliography: Bibliography::None,
            runtime: true,
        }
    }
}

fn selected_cases(requested: &[String]) -> Result<Vec<CorpusCase>> {
    if requested.is_empty() {
        return Ok(CORPUS_CASES.to_vec());
    }
    let mut selected = Vec::new();
    for name in requested {
        let Some(case) = CORPUS_CASES.iter().find(|case| case.name == name) else {
            return Err(message(format!(
                "unknown corpus case {name}; expected one of {}",
                CORPUS_CASES
                    .iter()
                    .map(|case| case.name)
                    .collect::<Vec<_>>()
                    .join(", ")
            )));
        };
        if !selected
            .iter()
            .any(|selected: &CorpusCase| selected.name == name)
        {
            selected.push(*case);
        }
    }
    Ok(selected)
}

#[derive(Serialize)]
struct CorpusMetric {
    project: String,
    providers: usize,
    static_scan_misses: usize,
    convergence_rounds: usize,
    unwanted_providers: usize,
    lock_bytes: u64,
    store_bytes: u64,
}

fn corpus_metric(
    project: &str,
    lock_path: &Path,
    store: &Path,
    misses: usize,
    rounds: usize,
) -> Result<CorpusMetric> {
    let lock: LockFile = read_json(lock_path)?;
    let unwanted = lock
        .closure
        .iter()
        .filter(|package| {
            let provider = package.provider.as_str();
            provider.starts_with("collection-")
                || provider.starts_with("scheme-")
                || provider.ends_with(".windows")
                || provider.ends_with(".x86_64-linux")
        })
        .count();
    Ok(CorpusMetric {
        project: project.to_string(),
        providers: lock.closure.len(),
        static_scan_misses: misses,
        convergence_rounds: rounds,
        unwanted_providers: unwanted,
        lock_bytes: fs::metadata(lock_path)?.len(),
        store_bytes: directory_bytes(store)?,
    })
}

struct TexLive {
    dist: PathBuf,
    sysvar: PathBuf,
    sysconfig: PathBuf,
    var: Option<PathBuf>,
    config: Option<PathBuf>,
    cnf_directories: Vec<PathBuf>,
}

impl TexLive {
    fn discover() -> Result<Self> {
        let dist = kpse_directory("TEXMFDIST", true)?
            .ok_or_else(|| message("kpsewhich returned no TEXMFDIST"))?;
        let sysvar = kpse_directory("TEXMFSYSVAR", true)?
            .ok_or_else(|| message("kpsewhich returned no TEXMFSYSVAR"))?;
        let sysconfig = kpse_directory("TEXMFSYSCONFIG", true)?
            .ok_or_else(|| message("kpsewhich returned no TEXMFSYSCONFIG"))?;
        let var = kpse_directory("TEXMFVAR", false)?;
        let config = kpse_directory("TEXMFCONFIG", false)?;
        let mut command = Command::new("kpsewhich");
        command.args(["-all", "texmf.cnf"]);
        let mut cnf_directories = capture(&mut command)?
            .lines()
            .map(str::trim)
            .filter(|line| !line.is_empty())
            .map(PathBuf::from)
            .filter(|path| path.is_file())
            .filter_map(|path| path.parent().map(Path::to_path_buf))
            .collect::<Vec<_>>();
        deduplicate_paths(&mut cnf_directories);
        Ok(Self {
            dist,
            sysvar,
            sysconfig,
            var,
            config,
            cnf_directories,
        })
    }

    fn convergence_engine_roots(&self) -> Vec<PathBuf> {
        let mut roots = vec![
            self.dist.join("fonts"),
            self.dist.join("tex/latex/l3backend"),
            self.dist.join("web2c"),
            self.sysvar.clone(),
            self.sysconfig.clone(),
        ];
        roots.extend(self.cnf_directories.iter().cloned());
        roots.extend(self.var.iter().cloned());
        roots.extend(self.config.iter().cloned());
        roots.retain(|path| path.is_dir());
        deduplicate_paths(&mut roots);
        roots
    }

    fn corpus_engine_roots(&self, extra: Option<&Path>) -> Vec<PathBuf> {
        let mut roots = vec![
            self.dist.join("fonts"),
            self.dist.join("tex/generic/unicode-data"),
            self.dist.join("tex/latex/l3backend"),
            self.dist.join("tex/latex/tex-ini-files"),
            self.dist.join("web2c"),
            self.sysvar.clone(),
            self.sysconfig.clone(),
        ];
        roots.extend(self.var.iter().cloned());
        roots.extend(self.config.iter().cloned());
        roots.extend(self.cnf_directories.iter().cloned());
        roots.extend(extra.map(Path::to_path_buf));
        roots.retain(|path| path.is_dir());
        deduplicate_paths(&mut roots);
        roots
    }
}

fn kpse_directory(name: &str, required: bool) -> Result<Option<PathBuf>> {
    let mut command = Command::new("kpsewhich");
    command.arg(format!("-var-value={name}"));
    let value = capture(&mut command)?;
    let value = value.trim();
    if value.is_empty() {
        if required {
            return Err(message(format!("kpsewhich returned an empty {name}")));
        }
        return Ok(None);
    }
    let path = PathBuf::from(value);
    if path.is_dir() {
        Ok(Some(path))
    } else if required {
        Err(message(format!(
            "kpsewhich returned an invalid {name}: {}",
            path.display()
        )))
    } else {
        Ok(None)
    }
}

fn deduplicate_paths(paths: &mut Vec<PathBuf>) {
    let mut seen = BTreeSet::new();
    paths.retain(|path| seen.insert(path.clone()));
}
