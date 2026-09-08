use std::fs;
use std::io::{Read, Write as _};
use std::path::{Path, PathBuf};

use clap::Parser;
use pqty_fls::{AdapterError, RootMapping, TraceScope, adapt_fls};
use serde::Deserialize;

const MAX_INPUT_BYTES: u64 = 64 * 1024 * 1024;

const ENVIRONMENT_SCHEMA: &str = "pqty.env/v1";

#[derive(Debug, Deserialize)]
struct EnvironmentIdentity {
    schema: String,
    fingerprint: String,
}

#[derive(Debug, Parser)]
#[command(version, about = "Convert a TeX recorder .fls file into pqty.trace/v1")]
struct Cli {
    /// TeX recorder file produced by an engine running with `-recorder`.
    #[arg(long)]
    fls: PathBuf,
    /// `pqty.env/v1` mounted for the recorded run. Used to stamp its
    /// fingerprint without coupling the adapter to pqty's Rust crate.
    #[arg(long, conflicts_with = "environment_fingerprint")]
    environment: Option<PathBuf>,
    /// Environment fingerprint to stamp when no environment document is
    /// available.
    #[arg(long, conflicts_with = "environment")]
    environment_fingerprint: Option<String>,
    /// Project source root.
    #[arg(long)]
    project_root: PathBuf,
    /// Package TEXMF root. Repeat for pqty and discovery roots.
    #[arg(long, required = true)]
    package_root: Vec<PathBuf>,
    /// Engine-owned root. Repeat for formats, configuration, and baseline
    /// resources.
    #[arg(long)]
    engine_root: Vec<PathBuf>,
    /// Build output root. A separate output directory is required so recorder
    /// inputs can be classified unambiguously.
    #[arg(long, required = true)]
    output_root: Vec<PathBuf>,
    /// Write the trace here instead of stdout.
    #[arg(short, long)]
    output: Option<PathBuf>,
}

fn main() {
    if let Err(error) = run(Cli::parse()) {
        eprintln!("pqty-fls: {error}");
        std::process::exit(1);
    }
}

fn run(cli: Cli) -> Result<(), AdapterError> {
    let current = std::env::current_dir().map_err(|source| AdapterError::Io {
        path: PathBuf::from("."),
        source,
    })?;
    let project = RootMapping::new(&cli.project_root, TraceScope::Project, &current)?;
    let mut roots = vec![project.clone()];
    for root in cli.package_root {
        roots.push(RootMapping::new(root, TraceScope::Package, &current)?);
    }
    for root in cli.engine_root {
        roots.push(RootMapping::new(root, TraceScope::Engine, &current)?);
    }
    for root in cli.output_root {
        roots.push(RootMapping::new(root, TraceScope::Output, &current)?);
    }

    let fingerprint = match cli.environment {
        Some(path) => {
            let text = read_text(&path)?;
            let environment: EnvironmentIdentity = serde_json::from_str(&text)?;
            if environment.schema != ENVIRONMENT_SCHEMA {
                return Err(AdapterError::Usage(format!(
                    "unsupported environment schema {}; expected {ENVIRONMENT_SCHEMA}",
                    environment.schema
                )));
            }
            Some(environment.fingerprint)
        }
        None => cli.environment_fingerprint,
    };
    let contents = read_text(&cli.fls)?;
    let trace = adapt_fls(
        &contents,
        &project.root,
        &roots,
        Some(format!("pqty-fls/{}", env!("CARGO_PKG_VERSION"))),
        fingerprint,
    )?;
    let mut bytes = serde_json::to_vec_pretty(&trace)?;
    bytes.push(b'\n');
    match cli.output {
        Some(path) => atomic_write(&path, &bytes),
        None => std::io::stdout()
            .write_all(&bytes)
            .map_err(|source| AdapterError::Io {
                path: PathBuf::from("<stdout>"),
                source,
            }),
    }
}

fn read_text(path: &Path) -> Result<String, AdapterError> {
    let file = fs::File::open(path).map_err(|source| AdapterError::Io {
        path: path.to_path_buf(),
        source,
    })?;
    read_text_bounded(file, path, MAX_INPUT_BYTES)
}

fn read_text_bounded(reader: impl Read, path: &Path, limit: u64) -> Result<String, AdapterError> {
    let mut bytes = Vec::new();
    reader
        .take(limit.saturating_add(1))
        .read_to_end(&mut bytes)
        .map_err(|source| AdapterError::Io {
            path: path.to_path_buf(),
            source,
        })?;
    if bytes.len() as u64 > limit {
        return Err(AdapterError::Usage(format!(
            "input {} exceeds the supported {limit} byte limit",
            path.display()
        )));
    }
    String::from_utf8(bytes)
        .map_err(|_| AdapterError::Usage(format!("input {} is not valid UTF-8", path.display())))
}

fn atomic_write(path: &Path, bytes: &[u8]) -> Result<(), AdapterError> {
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| {
            AdapterError::Usage(format!(
                "output path must name a concrete file: {}",
                path.display()
            ))
        })?;
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |duration| duration.as_nanos());
    let temporary = parent.join(format!(
        ".{name}.pqty-fls-{}.{}.tmp",
        std::process::id(),
        nonce
    ));
    let result = (|| -> Result<(), AdapterError> {
        let mut file = fs::OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&temporary)
            .map_err(|source| AdapterError::Io {
                path: temporary.clone(),
                source,
            })?;
        file.write_all(bytes).map_err(|source| AdapterError::Io {
            path: temporary.clone(),
            source,
        })?;
        file.sync_all().map_err(|source| AdapterError::Io {
            path: temporary.clone(),
            source,
        })?;
        fs::rename(&temporary, path).map_err(|source| AdapterError::Io {
            path: path.to_path_buf(),
            source,
        })
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;
    use std::path::Path;

    #[test]
    fn input_reads_accept_the_limit_and_reject_oversized_or_non_utf8_data() {
        let path = Path::new("main.fls");
        assert_eq!(
            super::read_text_bounded(Cursor::new(b"INPUT"), path, 5).expect("exact limit"),
            "INPUT"
        );
        let mut reader = Cursor::new(b"INPUT main.tex");
        let error = super::read_text_bounded(&mut reader, path, 5).expect_err("oversized input");
        assert_eq!(reader.position(), 6, "must stop reading at limit + 1");
        assert!(error.to_string().contains("main.fls"));
        assert!(
            super::read_text_bounded(Cursor::new([0xff]), path, 5)
                .expect_err("non-UTF8 input")
                .to_string()
                .contains("UTF-8")
        );
    }
}
