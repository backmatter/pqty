use std::fs;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};

use serde::Deserialize;

use crate::cli::args::RemoteSelector;
use crate::{
    DEFAULT_TLNET, MetadataCachePolicy, PqtyError, RegistryRequest, TEXLIVE_ARCHIVE_ROOT,
    default_store_dir,
};

/// Optional `pqty.toml` providing project defaults so common flags need not be
/// repeated. CLI flags always win; the config fills in what they omit.
#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Config {
    #[serde(default)]
    registry: RegistryConfig,
    #[serde(default)]
    store: StoreConfig,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct RegistryConfig {
    /// tlnet tlpdb URL to resolve/fetch from.
    url: Option<String>,
    /// Rolling (`latest`) or immutable dated (`YYYY-MM-DD`) Registry Snapshot.
    remote: Option<RemoteSelector>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct StoreConfig {
    /// Content-addressable store directory.
    path: Option<PathBuf>,
}

impl Config {
    pub(super) fn load() -> Result<Self, PqtyError> {
        Self::load_from(Path::new("pqty.toml"))
    }

    fn load_from(path: &Path) -> Result<Self, PqtyError> {
        match fs::read_to_string(path) {
            Ok(text) => toml::from_str(&text).map_err(|source| PqtyError::Toml {
                path: path.to_path_buf(),
                source: Box::new(source),
            }),
            Err(source) if source.kind() == ErrorKind::NotFound => Ok(Config::default()),
            Err(source) => Err(PqtyError::Io {
                path: path.to_path_buf(),
                source,
            }),
        }
    }

    /// Effective Registry Snapshot request from CLI flags merged with config.
    /// CLI options win over project configuration.
    pub(super) fn registry_request(
        &self,
        cli_url: Option<String>,
        cli_remote: Option<RemoteSelector>,
        offline: bool,
        allow_insecure: bool,
    ) -> Result<Option<RegistryRequest>, PqtyError> {
        if let Some(url) = cli_url {
            let request = RegistryRequest {
                url,
                snapshot: None,
                cache_policy: if offline {
                    MetadataCachePolicy::Offline
                } else {
                    MetadataCachePolicy::Revalidate
                },
                allow_insecure,
            };
            request.validate()?;
            return Ok(Some(request));
        }
        if let Some(selector) = cli_remote {
            return selector.registry_request(offline, allow_insecure).map(Some);
        }
        if let Some(url) = self.registry.url.clone() {
            let request = RegistryRequest {
                url,
                snapshot: None,
                cache_policy: if offline {
                    MetadataCachePolicy::Offline
                } else {
                    MetadataCachePolicy::Revalidate
                },
                allow_insecure,
            };
            request.validate()?;
            return Ok(Some(request));
        }
        self.registry.remote.clone().map_or(Ok(None), |selector| {
            selector.registry_request(offline, allow_insecure).map(Some)
        })
    }

    pub(super) fn store_dir(&self, cli_store: Option<PathBuf>) -> PathBuf {
        cli_store
            .or_else(|| self.store.path.clone())
            .unwrap_or_else(default_store_dir)
    }
}

impl RemoteSelector {
    pub(super) fn registry_request(
        self,
        offline: bool,
        allow_insecure: bool,
    ) -> Result<RegistryRequest, PqtyError> {
        match self {
            Self::Latest if offline => Err(PqtyError::Usage(
                "`--offline` cannot be combined with `--remote latest`; select a dated Registry Snapshot"
                    .to_string(),
            )),
            Self::Latest => Ok(RegistryRequest {
                url: format!("{DEFAULT_TLNET}/tlpkg/texlive.tlpdb.xz"),
                snapshot: None,
                cache_policy: MetadataCachePolicy::Revalidate,
                allow_insecure,
            }),
            Self::Dated(date) => {
                let (year, month, day) = (&date[0..4], &date[5..7], &date[8..10]);
                Ok(RegistryRequest {
                    url: format!(
                        "{TEXLIVE_ARCHIVE_ROOT}/{year}/{month}/{day}/tlnet/tlpkg/texlive.tlpdb.xz"
                    ),
                    snapshot: Some(date),
                    cache_policy: if offline {
                        MetadataCachePolicy::Offline
                    } else {
                        MetadataCachePolicy::Immutable
                    },
                    allow_insecure,
                })
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::PathBuf;

    use crate::cli::config::Config;

    fn temporary_directory(label: &str) -> PathBuf {
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "pqty-config-{label}-{}-{nonce}",
            std::process::id()
        ));
        fs::create_dir(&path).expect("temporary directory");
        path
    }

    #[test]
    fn missing_configuration_uses_defaults() {
        let directory = temporary_directory("missing");
        let config =
            Config::load_from(&directory.join("absent.toml")).expect("missing config is optional");
        assert!(config.registry.url.is_none());
        assert!(config.registry.remote.is_none());
        assert!(config.store.path.is_none());
        fs::remove_dir_all(directory).expect("cleanup");
    }

    #[test]
    fn invalid_or_unknown_configuration_is_an_error() {
        let directory = temporary_directory("invalid");
        let path = directory.join("pqty.toml");
        fs::write(&path, "[registry]\nremtoe = \"latest\"\n").expect("config");
        let error = Config::load_from(&path).expect_err("unknown key");
        assert!(error.to_string().contains("unknown field"));

        fs::write(&path, "[registry\n").expect("invalid config");
        Config::load_from(&path).expect_err("invalid TOML");
        fs::remove_dir_all(directory).expect("cleanup");
    }
}
