//! What a migration has already done.
//!
//! A migration of a real repository moves terabytes and can run for hours, so
//! it has to survive being interrupted — a dropped connection, a full disk, a
//! laptop lid. That means every unit of work is recorded the moment it is
//! finished, and a second run reads the record and starts where the first
//! stopped.
//!
//! The records are append-only text. Not because text is elegant, but because
//! an append of one short line is the only write that is atomic enough to be
//! trusted at the moment a process is killed: a partly written final line is
//! discarded on read, and everything before it is still true. A structured
//! document rewritten after each object would be both slower and less safe.
//!
//! Phase outputs that are computed all at once rather than incrementally — the
//! remote listing, the survey — are whole files written atomically instead.

use std::collections::{HashMap, HashSet};
use std::fs::{self, File, OpenOptions};
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};

use crate::{io_error, write_atomic, Failure};

/// Bumped when a record's meaning changes, so an old journal is restarted
/// rather than misread.
const FORMAT: u32 = 1;

/// The phases a migration runs through, in order.
///
/// Recorded on completion, so a resumed run can skip a whole phase rather than
/// re-deriving what it already knows.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Phase {
    Inventory,
    Survey,
    Transfer,
    Manifests,
    Replay,
    Refs,
}

impl Phase {
    pub(crate) fn name(self) -> &'static str {
        match self {
            Phase::Inventory => "inventory",
            Phase::Survey => "survey",
            Phase::Transfer => "transfer",
            Phase::Manifests => "manifests",
            Phase::Replay => "replay",
            Phase::Refs => "refs",
        }
    }
}

/// One object the migration has to move.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct Needed {
    pub(crate) md5: String,
    pub(crate) size: u64,
    /// Whether this is a DVC directory manifest rather than artifact content.
    pub(crate) directory: bool,
}

/// The AVC manifest that replaced a DVC directory manifest.
#[derive(Clone, Debug)]
pub(crate) struct Manifest {
    pub(crate) hash: String,
    pub(crate) size: u64,
}

pub(crate) struct Journal {
    root: PathBuf,
    /// Objects confirmed present on the destination remote.
    stored: HashSet<String>,
    /// DVC directory manifest md5 to the AVC manifest that replaced it.
    manifests: HashMap<String, Manifest>,
    /// Original commit to rewritten commit.
    commits: HashMap<String, String>,
    /// DVC md5 to the SHA-256 it was re-hashed to, under `--rehash`.
    rehashed: HashMap<String, String>,
    completed: HashSet<String>,
    /// Held open across a phase: reopening per record would dominate the cost
    /// of recording one.
    log: Option<BufWriter<File>>,
}

impl Journal {
    /// Open the journal beneath `state`, verifying it describes this migration.
    ///
    /// `fingerprint` is what the run was asked to do. A journal recording
    /// something else is refused rather than resumed, because resuming it would
    /// silently mix two migrations into one repository.
    pub(crate) fn open(state: &Path, fingerprint: &str, restart: bool) -> Result<Self, Failure> {
        let root = state.to_path_buf();
        if restart && root.exists() {
            fs::remove_dir_all(&root).map_err(io_error)?;
        }
        fs::create_dir_all(&root).map_err(io_error)?;

        let stamp = root.join("migration");
        let expected = format!("{FORMAT}\n{fingerprint}\n");
        match fs::read_to_string(&stamp) {
            Ok(found) if found == expected => {}
            Ok(_) => {
                return Err(format!(
                    "{} records a different migration; finish or remove it, or pass --restart \
                     to discard it and start over",
                    root.display()
                )
                .into())
            }
            Err(_) => write_atomic(&stamp, expected.as_bytes())?,
        }

        let mut journal = Self {
            root,
            stored: HashSet::new(),
            manifests: HashMap::new(),
            commits: HashMap::new(),
            rehashed: HashMap::new(),
            completed: HashSet::new(),
            log: None,
        };
        journal.replay()?;
        Ok(journal)
    }

    /// Read every record written by an earlier run.
    fn replay(&mut self) -> Result<(), Failure> {
        for line in read_lines(&self.root.join("progress.log"))? {
            let mut fields = line.split('\t');
            match (fields.next(), fields.next()) {
                (Some("stored"), Some(hash)) => {
                    self.stored.insert(hash.to_owned());
                }
                (Some("manifest"), Some(source)) => {
                    let (Some(hash), Some(size)) = (fields.next(), fields.next()) else {
                        continue;
                    };
                    let Ok(size) = size.parse() else { continue };
                    self.manifests.insert(
                        source.to_owned(),
                        Manifest {
                            hash: hash.to_owned(),
                            size,
                        },
                    );
                }
                (Some("rehashed"), Some(md5)) => {
                    let Some(hash) = fields.next() else { continue };
                    self.rehashed.insert(md5.to_owned(), hash.to_owned());
                }
                (Some("commit"), Some(old)) => {
                    let Some(new) = fields.next() else { continue };
                    self.commits.insert(old.to_owned(), new.to_owned());
                }
                (Some("phase"), Some(name)) => {
                    self.completed.insert(name.to_owned());
                }
                // A line torn in half by a kill, or written by a future
                // version. Everything before it still stands.
                _ => continue,
            }
        }
        Ok(())
    }

    /// Append one record and get it onto the disk.
    ///
    /// Flushed but not `fsync`ed: losing the last few records to a power cut
    /// costs a re-transfer of those objects, which the next run detects and
    /// redoes, while an `fsync` per object would make the journal slower than
    /// the work it records.
    fn record(&mut self, line: &str) -> Result<(), Failure> {
        if self.log.is_none() {
            let file = OpenOptions::new()
                .create(true)
                .append(true)
                .open(self.root.join("progress.log"))
                .map_err(io_error)?;
            self.log = Some(BufWriter::new(file));
        }
        let log = self.log.as_mut().expect("just opened");
        writeln!(log, "{line}").map_err(io_error)?;
        log.flush().map_err(io_error)?;
        Ok(())
    }

    pub(crate) fn is_complete(&self, phase: Phase) -> bool {
        self.completed.contains(phase.name())
    }

    pub(crate) fn complete(&mut self, phase: Phase) -> Result<(), Failure> {
        self.completed.insert(phase.name().to_owned());
        self.record(&format!("phase\t{}", phase.name()))
    }

    pub(crate) fn is_stored(&self, hash: &str) -> bool {
        self.stored.contains(hash)
    }

    pub(crate) fn mark_stored(&mut self, hash: &str) -> Result<(), Failure> {
        self.stored.insert(hash.to_owned());
        self.record(&format!("stored\t{hash}"))
    }

    pub(crate) fn manifest(&self, source_md5: &str) -> Option<&Manifest> {
        self.manifests.get(source_md5)
    }

    pub(crate) fn mark_manifest(
        &mut self,
        source_md5: &str,
        hash: &str,
        size: u64,
    ) -> Result<(), Failure> {
        self.manifests.insert(
            source_md5.to_owned(),
            Manifest {
                hash: hash.to_owned(),
                size,
            },
        );
        self.record(&format!("manifest\t{source_md5}\t{hash}\t{size}"))
    }

    pub(crate) fn commit(&self, original: &str) -> Option<&String> {
        self.commits.get(original)
    }

    pub(crate) fn mark_commit(&mut self, original: &str, rewritten: &str) -> Result<(), Failure> {
        self.commits
            .insert(original.to_owned(), rewritten.to_owned());
        self.record(&format!("commit\t{original}\t{rewritten}"))
    }

    /// Where a DVC directory manifest's bytes are kept once downloaded.
    ///
    /// Cached on disk because the replay needs them again, one commit at a
    /// time, long after the transfer phase has finished; re-downloading a
    /// manifest per commit would put the network back in a loop that should be
    /// local.
    pub(crate) fn dir_manifest_path(&self, md5: &str) -> PathBuf {
        self.root.join("dirs").join(md5)
    }

    pub(crate) fn save_dir_manifest(&self, md5: &str, bytes: &[u8]) -> Result<(), Failure> {
        write_atomic(&self.dir_manifest_path(md5), bytes)
    }

    pub(crate) fn load_dir_manifest(&self, md5: &str) -> Result<Vec<u8>, Failure> {
        fs::read(self.dir_manifest_path(md5)).map_err(|error| {
            Failure::from(format!(
                "the DVC directory manifest {md5} is missing from the migration state: {error}"
            ))
        })
    }

    /// Record the remote listing: every object key, with its size.
    pub(crate) fn save_inventory(&self, sizes: &HashMap<String, u64>) -> Result<(), Failure> {
        let mut text = String::with_capacity(sizes.len() * 42);
        // Sorted so two runs over one remote produce the same file, which
        // makes a diff of two migrations meaningful.
        let mut entries: Vec<_> = sizes.iter().collect();
        entries.sort();
        for (md5, size) in entries {
            text.push_str(&format!("{md5}\t{size}\n"));
        }
        write_atomic(&self.root.join("inventory.tsv"), text.as_bytes())
    }

    pub(crate) fn load_inventory(&self) -> Result<HashMap<String, u64>, Failure> {
        let mut sizes = HashMap::new();
        for line in read_lines(&self.root.join("inventory.tsv"))? {
            if let Some((md5, size)) = line.split_once('\t') {
                if let Ok(size) = size.parse() {
                    sizes.insert(md5.to_owned(), size);
                }
            }
        }
        Ok(sizes)
    }

    /// Record every object the history needs.
    pub(crate) fn save_survey(&self, needed: &[Needed]) -> Result<(), Failure> {
        let mut text = String::with_capacity(needed.len() * 44);
        for object in needed {
            text.push_str(&format!(
                "{}\t{}\t{}\n",
                object.md5,
                object.size,
                if object.directory { "dir" } else { "file" }
            ));
        }
        write_atomic(&self.root.join("survey.tsv"), text.as_bytes())
    }

    pub(crate) fn load_survey(&self) -> Result<Vec<Needed>, Failure> {
        let mut needed = Vec::new();
        for line in read_lines(&self.root.join("survey.tsv"))? {
            let mut fields = line.split('\t');
            let (Some(md5), Some(size), Some(kind)) = (fields.next(), fields.next(), fields.next())
            else {
                continue;
            };
            let Ok(size) = size.parse() else { continue };
            needed.push(Needed {
                md5: md5.to_owned(),
                size,
                directory: kind == "dir",
            });
        }
        Ok(needed)
    }

    /// Remember whether the destination had a history before this began.
    ///
    /// Decided once, on the first run, and read back on every resume. The
    /// answer decides what the migrated branches are called, and a migration
    /// that renamed its branches halfway through because it was interrupted
    /// would be worse than one that failed.
    pub(crate) fn save_destination_existing(&self, existing: bool) -> Result<(), Failure> {
        write_atomic(
            &self.root.join("destination"),
            if existing { b"existing" } else { b"new" },
        )
    }

    pub(crate) fn load_destination_existing(&self) -> Option<bool> {
        match fs::read_to_string(self.root.join("destination"))
            .ok()?
            .as_str()
        {
            "existing" => Some(true),
            "new" => Some(false),
            _ => None,
        }
    }

    /// Remember which key layout the source remote turned out to use.
    ///
    /// Detecting it costs a listing, which on a large remote is the expensive
    /// part of the first phase; a resumed run reads the answer instead.
    pub(crate) fn save_layout(&self, layout: &str) -> Result<(), Failure> {
        write_atomic(&self.root.join("layout"), layout.as_bytes())
    }

    pub(crate) fn load_layout(&self) -> Option<String> {
        fs::read_to_string(self.root.join("layout")).ok()
    }

    /// The SHA-256 an MD5-addressed object was re-hashed to.
    ///
    /// Only populated by `--rehash`, and only consulted when building the
    /// manifest for a directory whose files have already moved.
    pub(crate) fn rehashed(&self, md5: &str) -> Option<&String> {
        self.rehashed.get(md5)
    }

    pub(crate) fn mark_rehashed(&mut self, md5: &str, hash: &str) -> Result<(), Failure> {
        self.rehashed.insert(md5.to_owned(), hash.to_owned());
        self.record(&format!("rehashed\t{md5}\t{hash}"))
    }

    /// Remove the state directory once the migration has finished.
    pub(crate) fn discard(self) -> Result<(), Failure> {
        drop(self.log);
        fs::remove_dir_all(&self.root).map_err(|error| io_error(error).into())
    }
}

/// Read a file into lines, treating a missing file as empty.
fn read_lines(path: &Path) -> Result<Vec<String>, Failure> {
    match fs::read_to_string(path) {
        Ok(text) => Ok(text.lines().map(str::to_owned).collect()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
        Err(error) => Err(io_error(error).into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let path = std::env::temp_dir().join(format!(
            "avc-journal-{}-{name}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|since| since.as_nanos())
                .unwrap_or_default()
        ));
        let _ = fs::remove_dir_all(&path);
        path
    }

    #[test]
    fn work_recorded_by_one_run_is_seen_by_the_next() {
        let path = scratch("resume");
        let mut journal = Journal::open(&path, "fingerprint", false).unwrap();
        journal.mark_stored("aaaa").unwrap();
        journal.mark_manifest("dirmd5", "bbbb", 42).unwrap();
        journal.mark_commit("old", "new").unwrap();
        journal.mark_rehashed("md5", "sha").unwrap();
        journal.complete(Phase::Transfer).unwrap();
        drop(journal);

        let resumed = Journal::open(&path, "fingerprint", false).unwrap();
        assert!(resumed.is_stored("aaaa"));
        assert!(!resumed.is_stored("cccc"));
        assert_eq!(resumed.manifest("dirmd5").unwrap().size, 42);
        assert_eq!(resumed.commit("old").unwrap(), "new");
        assert_eq!(resumed.rehashed("md5").unwrap(), "sha");
        assert!(resumed.is_complete(Phase::Transfer));
        assert!(!resumed.is_complete(Phase::Replay));
        resumed.discard().unwrap();
    }

    #[test]
    fn a_half_written_final_record_is_discarded_not_misread() {
        let path = scratch("torn");
        let mut journal = Journal::open(&path, "fingerprint", false).unwrap();
        journal.mark_stored("aaaa").unwrap();
        drop(journal);
        // Exactly what a kill mid-write leaves behind.
        let log = path.join("progress.log");
        let mut text = fs::read_to_string(&log).unwrap();
        text.push_str("stor");
        fs::write(&log, text).unwrap();

        let resumed = Journal::open(&path, "fingerprint", false).unwrap();
        assert!(resumed.is_stored("aaaa"), "the complete record survives");
        resumed.discard().unwrap();
    }

    #[test]
    fn a_journal_for_another_migration_is_refused() {
        let path = scratch("mismatch");
        Journal::open(&path, "one", false).unwrap();
        let error = match Journal::open(&path, "two", false) {
            Err(error) => error,
            Ok(_) => panic!("a journal describing another migration must not be resumed"),
        };
        assert!(error.to_string().contains("--restart"));
        // Which is exactly what --restart is for.
        let restarted = Journal::open(&path, "two", true).unwrap();
        restarted.discard().unwrap();
    }

    #[test]
    fn phase_outputs_round_trip() {
        let path = scratch("phases");
        let journal = Journal::open(&path, "fingerprint", false).unwrap();
        let sizes = HashMap::from([("aa".to_owned(), 1_u64), ("bb".to_owned(), 2)]);
        journal.save_inventory(&sizes).unwrap();
        assert_eq!(journal.load_inventory().unwrap(), sizes);

        let needed = vec![
            Needed {
                md5: "aa".into(),
                size: 1,
                directory: false,
            },
            Needed {
                md5: "bb".into(),
                size: 2,
                directory: true,
            },
        ];
        journal.save_survey(&needed).unwrap();
        assert_eq!(journal.load_survey().unwrap(), needed);

        journal.save_dir_manifest("bb", b"[]").unwrap();
        assert_eq!(journal.load_dir_manifest("bb").unwrap(), b"[]");
        // Nothing recorded reads as empty rather than as an error.
        assert!(journal.load_dir_manifest("missing").is_err());
        journal.discard().unwrap();
    }
}
