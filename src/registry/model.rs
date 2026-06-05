use std::path::Path;

use crate::{Registry, normalize_runfile};

// Resolver (A-path: TeX Live tlpdb).
//
// The resolver talks to a `PackageRegistry`, never to tlpdb directly, so the
// B-path (a CTAN-derived index) is another implementation and not a rewrite.
// ---------------------------------------------------------------------------

/// Distribution-agnostic package index. `TlpdbIndex` is the A-path; a future
/// CTAN-derived index implements the same trait.
pub trait PackageRegistry {
    /// Providers shipping `<stem>.<ext>` for the requested extensions.
    fn providers_of(&self, stem: &str, extensions: &[&str]) -> Vec<&str>;
    /// A unique provider shipping `<stem>.<ext>`, if the request is
    /// unambiguous.
    fn provider_of(&self, stem: &str, extensions: &[&str]) -> Option<&str> {
        let providers = self.providers_of(stem, extensions);
        (providers.len() == 1)
            .then(|| providers.first().copied())
            .flatten()
    }
    /// Providers shipping an exact requested basename such as `babel.def`.
    fn providers_of_file(&self, filename: &str) -> Vec<&str> {
        let path = Path::new(filename);
        let Some(stem) = path.file_stem().and_then(|stem| stem.to_str()) else {
            return Vec::new();
        };
        let Some(extension) = path.extension().and_then(|extension| extension.to_str()) else {
            return Vec::new();
        };
        self.providers_of(stem, &[extension])
    }
    /// A unique provider shipping an exact basename.
    fn provider_of_file(&self, filename: &str) -> Option<&str> {
        let providers = self.providers_of_file(filename);
        (providers.len() == 1)
            .then(|| providers.first().copied())
            .flatten()
    }
    /// Concrete OTF/TTF basenames and providers matching a font-loader stem.
    ///
    /// Consumers use this only for an explicitly typed `font-family` trace
    /// input, never as a case-insensitive fallback for ordinary TeX files.
    fn font_file_candidates<'a>(&'a self, _stem: &str) -> Vec<(&'a str, &'a str)> {
        Vec::new()
    }
    /// Providers shipping an exact normalized TDS path.
    fn providers_of_path(&self, _path: &str) -> Vec<&str> {
        Vec::new()
    }
    /// A unique provider shipping an exact normalized TDS path.
    fn provider_of_path(&self, path: &str) -> Option<&str> {
        let providers = self.providers_of_path(path);
        (providers.len() == 1)
            .then(|| providers.first().copied())
            .flatten()
    }
    /// Metadata (version + dependency edges) for a provider.
    fn package(&self, provider: &str) -> Option<&IndexPackage>;
    /// Registry record describing this index, for the lock.
    fn registry(&self) -> Registry;
}

/// Parsed metadata for one provider in a package registry.
#[derive(Debug, Clone)]
pub struct IndexPackage {
    /// Registry version or revision string used for audit output.
    pub version: String,
    /// TeX Live category (`Package`, `TLCore`, `Collection`, ...).
    pub category: String,
    /// Provider names declared as dependencies by the registry.
    pub depends: Vec<String>,
    /// `execute addMap` / `execute addMixedMap` registry actions.
    pub font_maps: Vec<String>,
    /// Runtime files (relative to TEXMF root), used by `materialize`.
    pub runfiles: Vec<String>,
    /// sha512 of the `.tar.xz` container. Present only in the tlnet tlpdb, not
    /// the installed one (which strips container metadata).
    pub container_checksum: Option<String>,
    /// Size of the container in bytes (tlnet tlpdb only).
    pub container_size: Option<u64>,
}

impl IndexPackage {
    pub(crate) fn belongs_to_package_layer(&self) -> bool {
        self.category == "Package" && self.provides_package_resource()
    }

    pub(super) fn provides_package_resource(&self) -> bool {
        matches!(self.category.as_str(), "Package" | "TLCore")
            && self.runfiles.iter().any(|path| {
                let (_, path) = normalize_runfile(path);
                matches!(
                    path.split('/').next(),
                    Some(
                        "tex"
                            | "bibtex"
                            | "fonts"
                            | "makeindex"
                            | "dvips"
                            | "dvipdfmx"
                            | "metafont"
                            | "metapost"
                            | "omega"
                    )
                )
            })
    }
}
