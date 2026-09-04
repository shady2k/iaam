//! What an instance has installed, where each profile came from, and why any
//! of them was refused.
//!
//! Two origins, one validator, one trust rule (decision 0019 §8).
//!
//! - **Bundled.** Profiles live in the repository under
//!   `crates/iaam-ingest/profiles/` and are compiled into the binary. They are
//!   reviewed like code, covered by fixtures invented end to end, and their
//!   integrity is the image's.
//! - **Local.** The operator may point the instance at a read-only directory of
//!   his own. It is how a profile for an institution nobody has shipped yet can
//!   be used without waiting for a release, and it is safe to allow precisely
//!   because a profile is data — nothing here is loaded into a process that
//!   then evaluates it.
//!
//! **A profile is accepted whole or not at all, and the unit is one profile.**
//! One unreadable file must not take the instance's other formats down with it.
//! What keeps the failure from being silent is that the catalogue names every
//! refused profile and why: a profile that is merely absent looks exactly like
//! one that was never written.
//!
//! **Integrity is the digest and the version binding, not a signature.** A
//! signature the owner makes with a key on the same host proves nothing about
//! the file that the file's presence does not. What integrity has to buy is the
//! ability to say which bytes wrote a fact, and [`SourceProfile::digest`] beside
//! [`SourceProfile::parser_version`] buys exactly that.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::verdict::Rejection;

use super::{SourceProfile, engine, load};

/// The T-Bank operations export, bundled.
///
/// Compiled in rather than copied into the image as a file, because "reviewed
/// like code" is a claim about the artefact and a file beside the binary can be
/// replaced without the binary changing. A bundled profile that fails to load
/// is a build defect, and there is a test that says so.
const BUNDLED: &[(&str, &str)] = &[(
    "tbank-operations-csv.json",
    include_str!("../../profiles/tbank-operations-csv.json"),
)];

/// Where a profile came from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Origin {
    /// Shipped in this build.
    Bundled { file: String },
    /// Read from the operator's own directory.
    Local { file: PathBuf },
}

impl Origin {
    /// Wire code. One place, so two routes cannot spell it differently.
    #[must_use]
    pub const fn code(&self) -> &'static str {
        match self {
            Self::Bundled { .. } => "bundled",
            Self::Local { .. } => "local",
        }
    }

    /// The file this profile was read from, for a human reading a catalogue.
    #[must_use]
    pub fn file(&self) -> String {
        match self {
            Self::Bundled { file } => file.clone(),
            Self::Local { file } => file.display().to_string(),
        }
    }
}

/// One profile this instance will read documents with.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Installed {
    pub profile: Arc<SourceProfile>,
    pub origin: Origin,
}

/// One file this instance will **not** read documents with, and why.
///
/// Published rather than logged. A profile that is merely absent looks exactly
/// like one that was never written, and the operator who mounted a directory of
/// his own is the one person who can tell the difference and fix it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Refused {
    pub origin: Origin,
    /// The profile's id where the file got far enough to state one.
    pub id: Option<String>,
    pub reason: String,
}

/// The format catalogue of one deployment.
///
/// A property of the deployment and not of the journal, which is why nothing
/// installs a profile through the API: two instances of one image must read one
/// institution's export the same way, and a per-journal catalogue would make an
/// export's reading depend on who uploaded what and when.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ProfileCatalogue {
    installed: Vec<Installed>,
    refused: Vec<Refused>,
}

impl ProfileCatalogue {
    /// Everything this build ships and nothing else.
    #[must_use]
    pub fn bundled() -> Self {
        let mut catalogue = Self::default();
        for (file, body) in BUNDLED {
            let origin = Origin::Bundled {
                file: (*file).to_owned(),
            };
            catalogue.admit(origin, load::from_bytes(body.as_bytes()));
        }
        catalogue
    }

    /// This build's profiles, plus the operator's own directory.
    ///
    /// A directory that cannot be read is one refusal naming the directory, not
    /// an empty catalogue: an operator who mounted the wrong path and an
    /// operator who mounted an empty one need different answers, and only the
    /// first is a mistake.
    ///
    /// Files are read in name order so that the catalogue an instance publishes
    /// does not depend on the order a filesystem happens to hand them back.
    #[must_use]
    pub fn with_local(directory: &Path) -> Self {
        let mut catalogue = Self::bundled();
        let entries = match std::fs::read_dir(directory) {
            Ok(entries) => entries,
            Err(error) => {
                catalogue.refused.push(Refused {
                    origin: Origin::Local {
                        file: directory.to_path_buf(),
                    },
                    id: None,
                    reason: format!("the profile directory could not be read: {error}"),
                });
                return catalogue;
            }
        };
        let mut files: Vec<PathBuf> = entries
            .filter_map(Result::ok)
            .map(|entry| entry.path())
            .filter(|path| {
                path.extension()
                    .is_some_and(|extension| extension == "json")
            })
            .collect();
        files.sort();
        for file in files {
            let origin = Origin::Local { file: file.clone() };
            let loaded = match std::fs::read(&file) {
                Ok(bytes) => load::from_bytes(&bytes),
                Err(error) => {
                    catalogue.refused.push(Refused {
                        origin,
                        id: None,
                        reason: format!("the file could not be read: {error}"),
                    });
                    continue;
                }
            };
            catalogue.admit(origin, loaded);
        }
        catalogue
    }

    /// Add one loaded-or-not profile, applying the collision rule.
    ///
    /// **A local profile whose id collides with a bundled one does not shadow
    /// it: it is refused**, and the catalogue publishes it as refused with the
    /// reason. Silence would mean an export read by a profile nobody chose.
    ///
    /// The same refusal covers two local files claiming one id, and covers the
    /// case decision 0019 §5 asks for from the other side: a version is a name
    /// for a content, so two files claiming one `(id, version)` with two
    /// different digests cannot both be installed — "the rows version 3 read"
    /// would not be a set, and a buggy profile's facts would not be findable.
    fn admit(&mut self, origin: Origin, loaded: Result<SourceProfile, load::ProfileError>) {
        let profile = match loaded {
            Ok(profile) => profile,
            Err(error) => {
                self.refused.push(Refused {
                    origin,
                    id: None,
                    reason: error.to_string(),
                });
                return;
            }
        };
        if let Some(standing) = self
            .installed
            .iter()
            .find(|installed| installed.profile.id() == profile.id())
        {
            self.refused.push(Refused {
                id: Some(profile.id().to_owned()),
                reason: format!(
                    "the id «{id}» is already installed from {origin} {file}, at version {version} \
                     and digest {digest}. A second profile does not shadow the first: an export \
                     read by a profile nobody chose is worse than an export nothing reads",
                    id = profile.id(),
                    origin = standing.origin.code(),
                    file = standing.origin.file(),
                    version = standing.profile.version(),
                    digest = standing.profile.digest(),
                ),
                origin,
            });
            return;
        }
        self.installed.push(Installed {
            profile: Arc::new(profile),
            origin,
        });
        self.installed
            .sort_by(|left, right| left.profile.id().cmp(right.profile.id()));
    }

    /// Every profile this instance reads documents with.
    #[must_use]
    pub fn installed(&self) -> &[Installed] {
        &self.installed
    }

    /// Every file this instance refused, and why.
    #[must_use]
    pub fn refused(&self) -> &[Refused] {
        &self.refused
    }

    /// The installed profile with this id.
    #[must_use]
    pub fn get(&self, id: &str) -> Option<&Installed> {
        self.installed
            .iter()
            .find(|installed| installed.profile.id() == id)
    }

    /// The one profile that recognises this document.
    ///
    /// **A document two profiles recognise is refused**, exactly as the report
    /// registry refuses a workbook two parsers recognise: two matches mean the
    /// criterion is too weak, and choosing either records facts read by the
    /// wrong profile.
    pub fn recognise(&self, bytes: &[u8]) -> Result<&Installed, Rejection> {
        let matched: Vec<&Installed> = self
            .installed
            .iter()
            .filter(|installed| engine::recognises(bytes, &installed.profile))
            .collect();
        match matched.as_slice() {
            [only] => Ok(only),
            [] => Err(Rejection {
                field: "document".to_owned(),
                expected: format!(
                    "a document one of this instance's source profiles recognises: {}",
                    self.catalogue_line()
                ),
                actual: "a document none of them recognises".to_owned(),
            }),
            several => Err(Rejection {
                field: "document".to_owned(),
                expected: "a document exactly one source profile recognises: name the \
                           profile to read it with"
                    .to_owned(),
                actual: format!(
                    "recognised by {}",
                    several
                        .iter()
                        .map(|installed| installed.profile.id().to_owned())
                        .collect::<Vec<_>>()
                        .join(", ")
                ),
            }),
        }
    }

    /// The installed profiles in one sentence, for a refusal somebody reads.
    fn catalogue_line(&self) -> String {
        if self.installed.is_empty() {
            return "this instance has no source profile installed".to_owned();
        }
        self.installed
            .iter()
            .map(|installed| {
                format!(
                    "{} ({})",
                    installed.profile.id(),
                    installed.profile.issuer()
                )
            })
            .collect::<Vec<_>>()
            .join(", ")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every profile this build ships loads.
    ///
    /// A bundled profile that does not load is a build defect and not an
    /// operator's problem, so it is caught here rather than published as a
    /// refusal on a running instance.
    #[test]
    fn every_bundled_profile_loads() {
        let catalogue = ProfileCatalogue::bundled();
        assert!(
            catalogue.refused().is_empty(),
            "a profile this build ships did not load: {:?}",
            catalogue.refused()
        );
        assert_eq!(catalogue.installed().len(), BUNDLED.len());
        for installed in catalogue.installed() {
            assert_eq!(installed.origin.code(), "bundled");
            assert!(
                installed.profile.parser_version().0.starts_with("profile/"),
                "the reserved prefix names the origin of a fact"
            );
            assert_eq!(installed.profile.digest().len(), 64);
        }
    }

    /// The T-Bank profile is installed under the id its parser version names.
    #[test]
    fn the_bundled_catalogue_carries_the_tbank_operations_export() {
        let catalogue = ProfileCatalogue::bundled();
        let installed = catalogue
            .get("tbank-operations-csv")
            .expect("the T-Bank operations export ships");
        assert_eq!(
            installed.profile.parser_version().0,
            format!(
                "profile/tbank-operations-csv/{}",
                installed.profile.version()
            )
        );
    }

    /// A second profile claiming an installed id does not shadow it: it is
    /// refused, and the refusal names what is already installed.
    ///
    /// Silence would mean an export read by a profile nobody chose. This is
    /// also the binding decision 0019 §5 asks for from the other side: two
    /// files claiming one `(id, version)` with two contents cannot both stand,
    /// or "the rows version 3 read" is not a set.
    #[test]
    fn a_colliding_id_is_refused_and_never_shadows() {
        let mut catalogue = ProfileCatalogue::bundled();
        let installed = catalogue.installed().len();
        let (file, body) = BUNDLED[0];
        catalogue.admit(
            Origin::Local {
                file: PathBuf::from(format!("/nowhere/{file}")),
            },
            load::from_bytes(body.as_bytes()),
        );
        assert_eq!(catalogue.installed().len(), installed);
        let refused = catalogue
            .refused()
            .last()
            .expect("the collision is published");
        assert_eq!(refused.origin.code(), "local");
        assert_eq!(refused.id.as_deref(), Some("tbank-operations-csv"));
        assert!(refused.reason.contains("already installed"), "{refused:?}");
    }

    /// An unreadable file is one refusal and takes nothing else down with it.
    ///
    /// A profile is accepted whole or not at all, and the unit of that rule is
    /// one profile: an instance whose other formats stopped working because of
    /// one bad file would be an instance whose operator cannot import anything
    /// until he finds it.
    #[test]
    fn one_unreadable_file_leaves_the_rest_installed() {
        let mut catalogue = ProfileCatalogue::bundled();
        let installed = catalogue.installed().len();
        catalogue.admit(
            Origin::Local {
                file: PathBuf::from("/nowhere/broken.json"),
            },
            load::from_bytes(b"{ this is not json"),
        );
        assert_eq!(catalogue.installed().len(), installed);
        assert_eq!(catalogue.refused().len(), 1);
        assert_eq!(catalogue.refused()[0].id, None);
    }

    /// A directory the operator named and the instance cannot read is one
    /// refusal naming the directory, not an empty catalogue.
    ///
    /// An operator who mounted the wrong path and one who mounted an empty
    /// directory need different answers, and only the first is a mistake.
    #[test]
    fn an_unreadable_directory_is_published_as_refused() {
        let catalogue = ProfileCatalogue::with_local(Path::new("/nowhere/at/all"));
        assert_eq!(catalogue.installed().len(), BUNDLED.len());
        let refused = catalogue.refused().first().expect("the directory is named");
        assert_eq!(refused.origin.code(), "local");
        assert!(refused.reason.contains("could not be read"), "{refused:?}");
    }

    /// A document no profile recognises is refused, and the refusal lists what
    /// this instance does read.
    #[test]
    fn a_document_nothing_recognises_is_refused_by_name() {
        let catalogue = ProfileCatalogue::bundled();
        let refusal = catalogue
            .recognise(b"date,type,account,amount,currency\n")
            .expect_err("iaam's own CSV is not an institution's export");
        assert_eq!(refusal.field, "document");
        assert!(
            refusal.expected.contains("tbank-operations-csv"),
            "{refusal:?}"
        );
    }
}
