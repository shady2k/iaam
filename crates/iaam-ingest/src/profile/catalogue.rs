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

use super::ledger::{Binding, VersionLedger};
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

    /// Bind every installed profile to the content its version names, refusing
    /// any profile whose version already names a different one.
    ///
    /// **This is the half of decision 0019 §5 that a single load cannot do.**
    /// [`Self::admit`] refuses two files claiming one id among the files of one
    /// pass; it says nothing about the file that was loaded *last time this
    /// instance started*. Without the ledger a profile edited between two
    /// starts is compared against nothing, and the new content is stamped on
    /// facts under the version the old content already stamped on others — at
    /// which point «the rows version 3 read» is not a set and a buggy profile's
    /// facts cannot be retracted as a group.
    ///
    /// **A changed content is refused; a changed version never is.** Raising
    /// the version is the whole supported way to change a reading, and this
    /// must not stand in its way: a new version is a new pair, and a new pair
    /// records and installs.
    ///
    /// A refused profile moves from installed to [`Self::refused`] with the
    /// reason naming both contents — the one the version stands for and the one
    /// it was handed — because a refusal without them leaves the operator
    /// comparing files by hand.
    ///
    /// A ledger that cannot be consulted refuses too. Not knowing whether the
    /// content changed is not knowing that it did not, and installing on the
    /// strength of an unanswered question is the silent acceptance this whole
    /// decision exists to refuse.
    #[must_use]
    pub fn bound_by(mut self, ledger: &mut dyn VersionLedger) -> Self {
        let mut bound = Vec::with_capacity(self.installed.len());
        for installed in std::mem::take(&mut self.installed) {
            let profile = &installed.profile;
            let answer = ledger.bind(profile.id(), profile.version(), profile.digest());
            let reason = match answer {
                Ok(Binding::Recorded | Binding::Unchanged) => {
                    bound.push(installed);
                    continue;
                }
                Ok(Binding::Differs { recorded }) => format!(
                    "version {version} of «{id}» already names the content {recorded} on this \
                     instance, and this file's content is {offered}. A version is a name for a \
                     content (decision 0019 §5): facts already recorded say they were read by \
                     this version, so a second content under it would make «the rows this \
                     version read» unanswerable. Change the reading under a new version",
                    version = profile.version(),
                    id = profile.id(),
                    offered = profile.digest(),
                ),
                Err(unavailable) => format!(
                    "{unavailable}, so whether version {version} of «{id}» still names the \
                     content this file carries is unknown. A profile is installed on a recorded \
                     answer and never on an unanswered question",
                    version = profile.version(),
                    id = profile.id(),
                ),
            };
            self.refused.push(Refused {
                id: Some(profile.id().to_owned()),
                reason,
                origin: installed.origin,
            });
        }
        self.installed = bound;
        self
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
    use std::collections::BTreeMap;
    use std::collections::btree_map::Entry;

    use iaam_core::ids::AccountId;

    use crate::csv_source::{AccountEntry, AccountNames};
    use crate::profile::ledger::LedgerUnavailable;

    use super::*;

    /// A ledger that outlives the catalogues bound against it, which is the
    /// whole of what is being tested here.
    ///
    /// It stands in for the instance's database and nothing else: it is built
    /// once per test, several catalogues are bound against it in turn, and each
    /// of those catalogues is a start of the instance. The durability itself —
    /// that the record survives a process and not merely a value — is proven
    /// against the real store, where the file is reopened.
    #[derive(Debug, Default)]
    struct RecordedVersions {
        bound: BTreeMap<(String, u32), String>,
        /// When set, every consultation fails, standing in for a database this
        /// instance cannot read.
        unavailable: bool,
    }

    impl VersionLedger for RecordedVersions {
        fn bind(
            &mut self,
            id: &str,
            version: u32,
            digest: &str,
        ) -> Result<Binding, LedgerUnavailable> {
            if self.unavailable {
                return Err(LedgerUnavailable(
                    "the database could not be read".to_owned(),
                ));
            }
            match self.bound.entry((id.to_owned(), version)) {
                Entry::Vacant(slot) => {
                    slot.insert(digest.to_owned());
                    Ok(Binding::Recorded)
                }
                Entry::Occupied(slot) if slot.get() == digest => Ok(Binding::Unchanged),
                Entry::Occupied(slot) => Ok(Binding::Differs {
                    recorded: slot.get().clone(),
                }),
            }
        }
    }

    /// One profile, invented end to end, with the version and one transcribed
    /// label the caller asks for.
    ///
    /// The schema's own example is the body, so the fixture is a profile this
    /// build genuinely loads; `document_label` is the field varied to change
    /// the file's content without changing what it reads, which is exactly the
    /// edit the binding has to catch — a changed content under a standing
    /// version is refused whether or not the change means anything.
    fn a_profile(version: u32, label: &str) -> Vec<u8> {
        let schema: serde_json::Value =
            serde_json::from_str(include_str!("../../schema/source-profile-v1.json"))
                .expect("the schema is JSON");
        let mut example = schema["examples"][0].clone();
        example["version"] = serde_json::json!(version);
        example["document_label"] = serde_json::json!(label);
        serde_json::to_vec(&example).expect("the fixture serialises")
    }

    /// One start of the instance: a catalogue holding just this file, bound
    /// against the ledger that outlives it.
    fn a_start(bytes: &[u8], ledger: &mut RecordedVersions) -> ProfileCatalogue {
        let mut catalogue = ProfileCatalogue::default();
        catalogue.admit(
            Origin::Local {
                file: PathBuf::from("/profiles/example.json"),
            },
            load::from_bytes(bytes),
        );
        catalogue.bound_by(ledger)
    }

    /// A version already recorded against one content refuses a second one, and
    /// it is the **record** that refuses: the catalogue that loaded the first
    /// content is gone by then.
    ///
    /// This is the defect the binding exists for, and it is not hypothetical: a
    /// wave changed what a bundled profile read and left the version at 1, and
    /// nothing mechanical noticed, because the only comparison in the tree was
    /// between two files loaded in one pass. A profile edited between two
    /// starts was compared against nothing at all.
    #[test]
    fn a_second_content_under_a_recorded_version_is_refused_after_a_restart() {
        let mut ledger = RecordedVersions::default();
        let first = a_profile(1, "Account statement");
        let first_digest = {
            let catalogue = a_start(&first, &mut ledger);
            let installed = catalogue
                .get("example-bank-statement")
                .expect("the first content installs");
            installed.profile.digest().to_owned()
            // and the catalogue is dropped here: nothing of this start
            // survives into the next but the ledger.
        };

        let changed = a_profile(1, "Statement of account");
        let catalogue = a_start(&changed, &mut ledger);

        assert!(
            catalogue.get("example-bank-statement").is_none(),
            "a version that already names a content does not take a second one"
        );
        let refused = catalogue
            .refused()
            .last()
            .expect("the refusal is published");
        assert_eq!(refused.id.as_deref(), Some("example-bank-statement"));
        assert!(
            refused.reason.contains(&first_digest),
            "the refusal names the content the version already stands for, or the operator \
             compares files by hand: {refused:?}"
        );
        let changed_digest = load::from_bytes(&changed)
            .expect("the changed file is a profile")
            .digest()
            .to_owned();
        assert!(
            refused.reason.contains(&changed_digest),
            "the refusal names the content it was handed as well: {refused:?}"
        );
    }

    /// A changed reading under a **new** version loads, and that is the whole
    /// supported way to change one.
    ///
    /// The binding refuses a changed content, never a changed version. A guard
    /// that stood in the way of an upgrade would be a guard nobody could ship a
    /// corrected profile past.
    #[test]
    fn a_profile_at_a_new_version_loads_normally() {
        let mut ledger = RecordedVersions::default();
        drop(a_start(&a_profile(1, "Account statement"), &mut ledger));

        let catalogue = a_start(&a_profile(2, "Statement of account"), &mut ledger);

        let installed = catalogue
            .get("example-bank-statement")
            .expect("a new version is how a reading changes");
        assert_eq!(installed.profile.version(), 2);
        assert!(catalogue.refused().is_empty(), "{:?}", catalogue.refused());
    }

    /// An unchanged profile loads on every start, and the binding is silent.
    ///
    /// An instance that refused its own profile the second time it started
    /// would refuse every import from then on, which is the opposite of what
    /// the record is for.
    #[test]
    fn an_unchanged_profile_loads_restart_after_restart() {
        let mut ledger = RecordedVersions::default();
        let body = a_profile(1, "Account statement");
        for _ in 0..3 {
            let catalogue = a_start(&body, &mut ledger);
            assert!(
                catalogue.get("example-bank-statement").is_some(),
                "the same content under the same version is the same profile"
            );
            assert!(catalogue.refused().is_empty(), "{:?}", catalogue.refused());
        }
    }

    /// Binding a catalogue records the digest of every profile it loads, which
    /// is the half of decision 0019 §5 that has to happen before anything can
    /// be refused.
    #[test]
    fn binding_records_the_content_of_every_profile_loaded() {
        let mut ledger = RecordedVersions::default();
        let catalogue = ProfileCatalogue::bundled().bound_by(&mut ledger);
        assert!(catalogue.refused().is_empty(), "{:?}", catalogue.refused());
        for installed in catalogue.installed() {
            assert_eq!(
                ledger
                    .bound
                    .get(&(
                        installed.profile.id().to_owned(),
                        installed.profile.version()
                    ))
                    .map(String::as_str),
                Some(installed.profile.digest()),
                "a profile this instance loaded is a profile whose content it recorded"
            );
        }
    }

    /// A ledger this instance cannot consult refuses the profile rather than
    /// installing it.
    ///
    /// Not knowing whether the content changed is not the same as knowing it
    /// did not, and installing on the strength of an unanswered question is the
    /// silent acceptance decision 0019 refuses. The refusal names the failure,
    /// because an operator whose database is unreadable needs to be told that
    /// and not that his profile is bad.
    #[test]
    fn a_ledger_that_cannot_be_consulted_refuses_rather_than_installs() {
        let mut ledger = RecordedVersions {
            unavailable: true,
            ..RecordedVersions::default()
        };

        let catalogue = a_start(&a_profile(1, "Account statement"), &mut ledger);

        assert!(catalogue.installed().is_empty());
        let refused = catalogue
            .refused()
            .last()
            .expect("the refusal is published");
        assert_eq!(refused.id.as_deref(), Some("example-bank-statement"));
        assert!(
            refused.reason.contains("could not be read"),
            "the refusal names why the ledger was silent: {refused:?}"
        );
    }

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

    /// A document carrying every heading a bundled profile recognises on, but
    /// missing one the profile **requires** in its row shape, is **recognised
    /// and then refused at read** — which is the wrong answer twice over.
    ///
    /// Recognition says "this profile reads this document" and the read then
    /// says it does not. The honest answer is the one the catalogue gives a
    /// document nothing reads, so recognition must be at least as demanding as
    /// the reading it promises.
    ///
    /// The rule runs the other way for a column the profile says the document
    /// may not print. Recognising on such a column would make its absence
    /// defeat recognition, which is the whole of what optionality was for; so a
    /// column that may be absent is exactly a column that identifies nothing.
    #[test]
    fn a_bundled_profile_recognises_on_every_column_it_requires() {
        for installed in ProfileCatalogue::bundled().installed() {
            let profile = &installed.profile;
            let recognised: Vec<&str> =
                profile.recognised_by().iter().map(String::as_str).collect();
            for required in profile.required_columns() {
                assert!(
                    recognised.contains(&required),
                    "«{required}» is a column «{id}» refuses a document for, and not one it \
                     recognises on: such a document is recognised and then refused at read, \
                     instead of falling through to a document no profile reads",
                    id = profile.id(),
                );
            }
            for optional in profile.optional_columns() {
                assert!(
                    !recognised.contains(&optional),
                    "«{optional}» is a column «{id}» reads where the document prints it and \
                     recognises on as well: an export without it is then refused as \
                     unrecognised, which is the refusal the column was made optional to avoid",
                    id = profile.id(),
                );
            }
        }
    }

    /// The T-Bank export prints the owner's own category and a standardised
    /// code, and an older export prints neither column at all. It is the same
    /// institution's statement either way, and it is read either way.
    ///
    /// Both columns are transcribed and neither decides anything: no direction,
    /// no amount, no day, no account. A month refused over one of them would be
    /// a month unreadable for a field no rule has to consult, so the profile
    /// says the document may not print them and the fields say the source
    /// printed none.
    ///
    /// An empty cell and a missing column reach the same reading, which is the
    /// point: `engine::transcribed` already answers "the source printed none",
    /// and this is that answer for a whole month.
    #[test]
    fn an_export_without_the_optional_columns_is_read_all_the_same() {
        let catalogue = ProfileCatalogue::bundled();
        // Invented end to end. The headings are the source's own printed
        // strings; every value under them is made up.
        let document = "Имя счёта;Номер карты;Дата операции;Сумма в валюте счёта;\
                        Валюта счёта;Статус;Категория по-умолчанию;Описание\n\
                        Main;*0000;05.08.2026 12:00:00;-1,00;RUB;Ок;Invented;Shop One\n";
        let installed = catalogue
            .recognise(document.as_bytes())
            .expect("an export printing neither optional column is still this profile's");
        assert_eq!(installed.profile.id(), "tbank-operations-csv");

        let account = AccountId::new_random();
        let names: AccountNames = [AccountEntry::titled("Main", account)]
            .into_iter()
            .collect();
        let reading = engine::read(
            document.as_bytes(),
            &installed.profile,
            &engine::ReadContext {
                accounts: &names,
                // The export prints its own account column.
                declared: None,
            },
        )
        .expect("a document this profile recognises is a document it reads");
        let [outcome] = reading.rows.as_slice() else {
            panic!("one line of data is one outcome: {:?}", reading.rows);
        };
        let engine::ReadOutcome::Observed { row, .. } = outcome else {
            panic!("the row is read: {outcome:?}");
        };
        assert_eq!(row.account, account);
        assert_eq!(row.amount_minor, -100);
        assert_eq!(row.source_category.as_deref(), Some("Invented"));
        // The two columns the document does not print say what an empty cell
        // says, and nothing else changes.
        assert_eq!(row.owner_category, None);
        assert_eq!(row.source_code, None);
    }

    /// The status column is required, and an export that does not print it is
    /// not this profile's document.
    ///
    /// With the column gone there is nothing to tell a movement the institution
    /// completed from one it did not, and reading every row as completed would
    /// write a fact the source never asserted. So this column is not among the
    /// ones that may be absent, and its absence is the refusal an operator can
    /// act on.
    #[test]
    fn an_export_without_the_status_column_is_not_this_profile_s() {
        let catalogue = ProfileCatalogue::bundled();
        let header = "Имя счёта;Номер карты;Дата операции;Сумма в валюте счёта;\
                      Валюта счёта;Категория по-умолчанию;Ваша категория;MCC;Описание\n";
        let refusal = catalogue
            .recognise(header.as_bytes())
            .expect_err("an export that prints no status column is not this profile's document");
        assert_eq!(refusal.field, "document");
        assert_eq!(refusal.actual, "a document none of them recognises");
    }
}
