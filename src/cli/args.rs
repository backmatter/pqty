use std::path::PathBuf;
use std::str::FromStr;

use clap::{Parser, Subcommand};
use serde::Deserialize;

use crate::{LinkMode, progress};

#[derive(Debug, Parser)]
#[command(
    version,
    about = "Engine-independent dependency and environment manager for LaTeX"
)]
pub(super) struct Cli {
    /// Filesystem root used for project-relative source paths. When omitted,
    /// the root TeX file's parent remains the standalone CLI default.
    #[arg(long, global = true)]
    pub(super) project_root: Option<PathBuf>,
    /// Additional project-relative input roots searched recursively after the
    /// source file's directory and project root.
    #[arg(long = "input-root", global = true)]
    pub(super) input_roots: Vec<PathBuf>,
    /// Ignore pqty.toml and use only explicit CLI arguments and platform
    /// defaults. Build-system consumers should set this.
    #[arg(long, global = true)]
    pub(super) no_config: bool,
    /// Do not access the network. Cached dated Registry Snapshots and package
    /// containers may still be used; the rolling `latest` selector is rejected.
    #[arg(long, global = true)]
    pub(super) offline: bool,
    /// Permit an explicit `http://` registry URL. HTTPS downgrade redirects remain forbidden.
    /// HTTPS provides transport integrity, not publisher authentication.
    #[arg(long, global = true)]
    pub(super) allow_insecure_registry: bool,
    /// Progress output written to stderr. Consumers should negotiate and select
    /// `json`; ordinary invocations default to human-readable progress.
    #[arg(long, global = true, value_enum, default_value_t = progress::ProgressOutput::Human)]
    pub(super) progress: progress::ProgressOutput,
    #[command(subcommand)]
    pub(super) command: Command,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(try_from = "String")]
pub(super) enum RemoteSelector {
    Latest,
    Dated(String),
}

impl FromStr for RemoteSelector {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        if value == "latest" {
            return Ok(Self::Latest);
        }
        if valid_iso_date(value) {
            return Ok(Self::Dated(value.to_string()));
        }
        Err("expected `latest` or a calendar date in YYYY-MM-DD form".to_string())
    }
}

impl TryFrom<String> for RemoteSelector {
    type Error = String;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        value.parse()
    }
}

fn valid_iso_date(value: &str) -> bool {
    let bytes = value.as_bytes();
    if bytes.len() != 10 || bytes[4] != b'-' || bytes[7] != b'-' {
        return false;
    }
    let Ok(year) = value[0..4].parse::<u16>() else {
        return false;
    };
    let Ok(month) = value[5..7].parse::<u8>() else {
        return false;
    };
    let Ok(day) = value[8..10].parse::<u8>() else {
        return false;
    };
    let leap = year.is_multiple_of(4) && (!year.is_multiple_of(100) || year.is_multiple_of(400));
    let days = match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if leap => 29,
        2 => 28,
        _ => return false,
    };
    day != 0 && day <= days
}

#[derive(Debug, Subcommand)]
pub(super) enum Command {
    /// Print the stable JSON and CLI protocols supported by this binary.
    Capabilities {},
    /// Print the dependency graph inferred from a root .tex file.
    Scan {
        /// Root TeX file to scan.
        root: PathBuf,
    },
    /// Explain what pqty inferred and what is still unresolved.
    Explain {
        /// Root TeX file to scan.
        root: PathBuf,
    },
    /// Print the resolved dependency closure as a tree.
    Tree {
        /// Root TeX file to scan.
        root: PathBuf,
        #[arg(long)]
        tlpdb: Option<PathBuf>,
        #[arg(long)]
        tlpdb_url: Option<String>,
        /// Select the rolling registry (`latest`) or an immutable dated
        /// Registry Snapshot (`YYYY-MM-DD`).
        #[arg(long, value_name = "latest|YYYY-MM-DD", conflicts_with_all = ["tlpdb", "tlpdb_url"])]
        remote: Option<RemoteSelector>,
    },
    /// Explain why a provider is in the closure (its dependency chains).
    Why {
        /// Root TeX file to scan.
        root: PathBuf,
        /// Provider (tlpdb package) to explain, e.g. `graphics`.
        provider: String,
        #[arg(long)]
        tlpdb: Option<PathBuf>,
        #[arg(long)]
        tlpdb_url: Option<String>,
        /// Select the rolling registry (`latest`) or an immutable dated
        /// Registry Snapshot (`YYYY-MM-DD`).
        #[arg(long, value_name = "latest|YYYY-MM-DD", conflicts_with_all = ["tlpdb", "tlpdb_url"])]
        remote: Option<RemoteSelector>,
    },
    /// Resolve scanned packages against a TeX Live package database (tlpdb).
    Resolve {
        /// Root TeX file to scan.
        root: PathBuf,
        /// Path to texlive.tlpdb. Auto-detected from the local TeX Live if omitted.
        #[arg(long)]
        tlpdb: Option<PathBuf>,
        /// URL of a tlnet texlive.tlpdb(.xz) to fetch and cache instead of the
        /// local install. Carries container checksums the local tlpdb lacks.
        #[arg(long)]
        tlpdb_url: Option<String>,
        /// Select the rolling registry (`latest`) or an immutable dated
        /// Registry Snapshot (`YYYY-MM-DD`).
        #[arg(long, value_name = "latest|YYYY-MM-DD", conflicts_with_all = ["tlpdb", "tlpdb_url"])]
        remote: Option<RemoteSelector>,
        /// Write the resolved lockfile instead of printing it.
        #[arg(short, long)]
        output: Option<PathBuf>,
    },
    /// Reproduce an existing lock's closure into a TEXMF tree, verifying each
    /// provider's integrity (fails on drift).
    Install {
        /// Lockfile to install from.
        #[arg(long, default_value = "pqty.lock")]
        lock: PathBuf,
        /// Content-addressable store directory. Defaults to the user cache dir.
        #[arg(long)]
        store: Option<PathBuf>,
        /// Output TEXMF tree to populate.
        #[arg(short = 'd', long, default_value = "pqty-texmf")]
        out: PathBuf,
        /// How to place files in the TEXMF tree from the store.
        #[arg(long, value_enum, default_value_t = LinkMode::Copy)]
        link: LinkMode,
    },
    /// Resolve and hash a complete package closure into a deterministic lockfile.
    Lock {
        /// Root TeX file to scan.
        root: PathBuf,
        /// Path to texlive.tlpdb. Auto-detected from the local TeX Live if omitted.
        #[arg(long)]
        tlpdb: Option<PathBuf>,
        /// Fetch metadata + containers from this tlnet URL instead of the local
        /// install.
        #[arg(long)]
        tlpdb_url: Option<String>,
        /// Select the rolling registry (`latest`) or an immutable dated
        /// Registry Snapshot (`YYYY-MM-DD`).
        #[arg(long, value_name = "latest|YYYY-MM-DD", conflicts_with_all = ["tlpdb", "tlpdb_url"])]
        remote: Option<RemoteSelector>,
        /// Require an exact runtime file in addition to source-discovered
        /// packages. May be repeated by toolchain/build-system consumers.
        #[arg(long = "require-file")]
        required_files: Vec<String>,
        /// Require a registry provider in addition to source-discovered
        /// packages. May be repeated; pqty remains unaware of why it is needed.
        #[arg(long = "require-provider")]
        required_providers: Vec<String>,
        /// Expected SHA-256 of the decompressed tlpdb metadata.
        #[arg(long)]
        tlpdb_sha256: Option<String>,
        /// Content-addressable store directory. Defaults to the user cache dir.
        #[arg(long)]
        store: Option<PathBuf>,
        /// Lockfile output path.
        #[arg(short, long, default_value = "pqty.lock")]
        output: PathBuf,
    },
    /// Print the engine-neutral environment manifest represented by a lock.
    Env {
        /// Fully materialized lockfile.
        #[arg(long, default_value = "pqty.lock")]
        lock: PathBuf,
    },
    /// Reconcile a generic runtime input trace with a locked environment.
    CheckTrace {
        /// Fully materialized lockfile.
        #[arg(long, default_value = "pqty.lock")]
        lock: PathBuf,
        /// JSON trace in the `pqty.trace/v1` format.
        #[arg(long)]
        trace: PathBuf,
    },
    /// Add package providers discovered by a runtime trace to an exact lock.
    Converge {
        /// Fully materialized lockfile to update.
        #[arg(long, default_value = "pqty.lock")]
        lock: PathBuf,
        /// JSON trace in the `pqty.trace/v1` format.
        #[arg(long)]
        trace: PathBuf,
        /// Path to the exact texlive.tlpdb used by the lock. By default pqty
        /// reloads the registry recorded in the lock.
        #[arg(long, conflicts_with = "tlpdb_url")]
        tlpdb: Option<PathBuf>,
        /// URL of the exact tlnet texlive.tlpdb(.xz) used by the lock. By
        /// default pqty reloads the registry recorded in the lock.
        #[arg(long, conflicts_with = "tlpdb")]
        tlpdb_url: Option<String>,
        /// Content-addressable store directory. Defaults to the user cache dir.
        #[arg(long)]
        store: Option<PathBuf>,
        /// Write the converged lock here. Defaults to updating `--lock`.
        #[arg(short, long)]
        output: Option<PathBuf>,
    },
    /// Add generic runtime files/providers to an exact lock.
    Require {
        /// Fully materialized lockfile to extend.
        #[arg(long, default_value = "pqty.lock")]
        lock: PathBuf,
        /// Runtime filename or normalized TDS path to require. May be repeated.
        #[arg(long = "file")]
        files: Vec<String>,
        /// Registry provider to require. May be repeated.
        #[arg(long = "provider")]
        providers: Vec<String>,
        /// Path to the exact texlive.tlpdb used by the lock.
        #[arg(long, conflicts_with = "tlpdb_url")]
        tlpdb: Option<PathBuf>,
        /// URL of the exact tlnet texlive.tlpdb used by the lock.
        #[arg(long, conflicts_with = "tlpdb")]
        tlpdb_url: Option<String>,
        /// Content-addressable store directory. Defaults to the user cache dir.
        #[arg(long)]
        store: Option<PathBuf>,
        /// Write the extended lock here. Defaults to updating `--lock`.
        #[arg(short, long)]
        output: Option<PathBuf>,
    },
}
