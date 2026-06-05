use std::ffi::{OsStr, OsString};
use std::fs::{self, File};
use std::io;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus, Stdio};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::de::DeserializeOwned;

use crate::{Result, message};

pub(crate) struct Binaries {
    pub(crate) pqty: PathBuf,
    pub(crate) pqty_fls: PathBuf,
}

impl Binaries {
    pub(crate) fn build(repo: &Path) -> Result<Self> {
        let mut build = cargo();
        build
            .current_dir(repo)
            .args(["build", "--quiet", "--locked", "--package", "pqty"])
            .args(["--package", "pqty-fls"]);
        run(&mut build)?;

        let mut metadata = cargo();
        metadata
            .current_dir(repo)
            .args(["metadata", "--no-deps", "--format-version", "1"]);
        let metadata: serde_json::Value = serde_json::from_str(&capture(&mut metadata)?)?;
        let target = metadata
            .get("target_directory")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| message("cargo metadata did not report target_directory"))?;
        let debug = Path::new(target).join("debug");
        let pqty = debug.join(binary_name("pqty"));
        let pqty_fls = debug.join(binary_name("pqty-fls"));
        for binary in [&pqty, &pqty_fls] {
            if !binary.is_file() {
                return Err(message(format!(
                    "cargo build did not produce {}",
                    binary.display()
                )));
            }
        }
        Ok(Self { pqty, pqty_fls })
    }
}

pub(crate) struct ScratchDir {
    path: PathBuf,
}

impl ScratchDir {
    pub(crate) fn new(label: &str) -> Result<Self> {
        let base = std::env::var_os("TMPDIR").map_or_else(std::env::temp_dir, PathBuf::from);
        let nonce = SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos();
        for attempt in 0..100_u8 {
            let path = base.join(format!(
                "pqty-{label}-{}-{nonce}-{attempt}",
                std::process::id()
            ));
            match fs::create_dir(&path) {
                Ok(()) => return Ok(Self { path }),
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
                Err(error) => return Err(error.into()),
            }
        }
        Err(message(format!(
            "could not create a unique pqty-{label} directory beneath {}",
            base.display()
        )))
    }

    pub(crate) fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for ScratchDir {
    fn drop(&mut self) {
        if let Err(error) = fs::remove_dir_all(&self.path)
            && error.kind() != io::ErrorKind::NotFound
        {
            eprintln!(
                "xtask: could not remove scratch directory {}: {error}",
                self.path.display()
            );
        }
    }
}

pub(crate) fn repo_root() -> Result<PathBuf> {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .map(Path::to_path_buf)
        .ok_or_else(|| message("xtask manifest has no repository parent"))
}

pub(crate) fn require_tools(tools: &[&str]) -> Result<()> {
    for tool in tools {
        if !on_path(tool) {
            return Err(message(format!("required tool is missing: {tool}")));
        }
    }
    Ok(())
}

pub(crate) fn run(command: &mut Command) -> Result<()> {
    let rendered = format!("{command:?}");
    let status = command.status()?;
    require_success(status, &rendered)
}

pub(crate) fn run_to_file(command: &mut Command, output: &Path) -> Result<()> {
    let file = File::create(output)?;
    command.stdout(Stdio::from(file));
    run(command)
}

pub(crate) fn run_to_files_allow_failure(
    command: &mut Command,
    output: &Path,
    error: &Path,
) -> Result<ExitStatus> {
    let rendered = format!("{command:?}");
    let status = command
        .stdout(Stdio::from(File::create(output)?))
        .stderr(Stdio::from(File::create(error)?))
        .status()?;
    if status.code().is_none() {
        return Err(message(format!(
            "{rendered} terminated without an exit code"
        )));
    }
    Ok(status)
}

pub(crate) fn capture(command: &mut Command) -> Result<String> {
    let rendered = format!("{command:?}");
    let output = command.output()?;
    require_success(output.status, &rendered)?;
    String::from_utf8(output.stdout).map_err(Into::into)
}

pub(crate) fn read_json<T: DeserializeOwned>(path: &Path) -> Result<T> {
    Ok(serde_json::from_reader(File::open(path)?)?)
}

pub(crate) fn write_json<T: serde::Serialize>(path: &Path, value: &T) -> Result<()> {
    let mut file = File::create(path)?;
    serde_json::to_writer_pretty(&mut file, value)?;
    use std::io::Write as _;
    writeln!(file)?;
    Ok(())
}

pub(crate) fn nonempty_file(path: &Path) -> Result<()> {
    let size = fs::metadata(path)?.len();
    if size == 0 {
        return Err(message(format!(
            "expected a nonempty file: {}",
            path.display()
        )));
    }
    Ok(())
}

pub(crate) fn directory_bytes(path: &Path) -> Result<u64> {
    let mut total = 0_u64;
    let mut pending = vec![path.to_path_buf()];
    while let Some(next) = pending.pop() {
        for entry in fs::read_dir(next)? {
            let entry = entry?;
            let metadata = entry.metadata()?;
            if metadata.is_dir() {
                pending.push(entry.path());
            } else if metadata.is_file() {
                total = total.saturating_add(metadata.len());
            }
        }
    }
    Ok(total)
}

pub(crate) fn search_path<I, P>(paths: I) -> Result<OsString>
where
    I: IntoIterator<Item = P>,
    P: AsRef<OsStr>,
{
    std::env::join_paths(paths).map_err(Into::into)
}

fn cargo() -> Command {
    Command::new(std::env::var_os("CARGO").unwrap_or_else(|| OsString::from("cargo")))
}

fn binary_name(name: &str) -> OsString {
    let mut binary = OsString::from(name);
    binary.push(std::env::consts::EXE_SUFFIX);
    binary
}

fn require_success(status: ExitStatus, rendered: &str) -> Result<()> {
    if status.success() {
        Ok(())
    } else {
        Err(message(format!("{rendered} exited with {status}")))
    }
}

fn on_path(tool: &str) -> bool {
    let Some(path) = std::env::var_os("PATH") else {
        return false;
    };
    let extensions = executable_extensions();
    std::env::split_paths(&path).any(|directory| {
        extensions
            .iter()
            .any(|extension| directory.join(format!("{tool}{extension}")).is_file())
    })
}

fn executable_extensions() -> Vec<String> {
    if cfg!(windows) {
        std::env::var_os("PATHEXT").map_or_else(
            || vec![".exe".to_string(), ".cmd".to_string(), ".bat".to_string()],
            |value| {
                value
                    .to_string_lossy()
                    .split(';')
                    .map(str::to_ascii_lowercase)
                    .collect()
            },
        )
    } else {
        vec![String::new()]
    }
}
