//! Reading DVC's own formats.
//!
//! Nothing here writes anything or talks to a network. It is the translation
//! layer: DVC's pointer files, its lock files, its directory manifests, and the
//! shape of its object keys, expressed as plain data so the rest of the
//! migration can work in AVC's terms.
//!
//! The parsing is deliberately permissive about fields it does not use — a
//! `.dvc` file carries stage metadata, descriptions, and labels that have no
//! bearing on where the bytes are — and deliberately strict about the ones it
//! does. An out whose hash is not an MD5 AVC can address is reported and
//! skipped, never guessed at.

use std::collections::BTreeMap;
use std::path::Path;

use serde::Deserialize;

use crate::Failure;

/// How many hexadecimal characters a DVC hash has.
const MD5_LENGTH: usize = 32;

/// The suffix DVC appends to the hash of a directory manifest.
pub(crate) const DIR_SUFFIX: &str = ".dir";

/// One piece of data DVC tracks, as AVC needs to see it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct Out {
    /// Repository-relative path, with any `..` in the DVC file already
    /// resolved.
    pub(crate) path: String,
    /// The 32-character MD5, without DVC's `.dir` suffix.
    pub(crate) md5: String,
    pub(crate) directory: bool,
    /// What DVC recorded, where it recorded anything. For a directory this is
    /// the total of the files beneath it, not the manifest's own length.
    pub(crate) size: Option<u64>,
}

/// One file inside a DVC directory manifest.
#[derive(Clone, Debug, Deserialize)]
pub(crate) struct DirEntry {
    pub(crate) md5: String,
    pub(crate) relpath: String,
}

/// Where a DVC remote puts its objects.
///
/// DVC 3 moved every object under `files/md5/`; before that the two-character
/// fan-out sat at the root of the remote. Both are still in the wild, and a
/// remote written by one and read by the other is the single most common way a
/// migration finds nothing, so the layout is detected rather than assumed.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, clap::ValueEnum)]
#[value(rename_all = "kebab-case")]
pub(crate) enum Layout {
    /// Probe the remote and use whichever layout has objects in it.
    #[default]
    Auto,
    /// DVC 3 and later: `files/md5/ab/cdef...`.
    FilesMd5,
    /// DVC 2 and earlier: `ab/cdef...`.
    Legacy,
}

impl Layout {
    /// The directory every object sits beneath, relative to the remote's own
    /// configured prefix.
    pub(crate) fn prefix(self) -> &'static str {
        match self {
            Layout::FilesMd5 | Layout::Auto => "files/md5/",
            Layout::Legacy => "",
        }
    }

    pub(crate) fn key(self, md5: &str, directory: bool) -> String {
        let suffix = if directory { DIR_SUFFIX } else { "" };
        format!("{}{}/{}{suffix}", self.prefix(), &md5[..2], &md5[2..])
    }

    pub(crate) fn label(self) -> &'static str {
        match self {
            Layout::Auto => "auto",
            Layout::FilesMd5 => "files/md5 (DVC 3)",
            Layout::Legacy => "two-character fan-out (DVC 2 and earlier)",
        }
    }

    /// Recover the MD5 a key addresses, ignoring anything shaped otherwise.
    ///
    /// Used to read a whole remote listing back into hashes, which is how the
    /// migration learns every object's size in one request rather than one
    /// request per object.
    pub(crate) fn md5_from_key(self, key: &str) -> Option<(String, bool)> {
        let rest = key.strip_prefix(self.prefix())?;
        let (fanout, tail) = rest.split_once('/')?;
        let (tail, directory) = match tail.strip_suffix(DIR_SUFFIX) {
            Some(stripped) => (stripped, true),
            None => (tail, false),
        };
        if fanout.len() != 2 || tail.len() != MD5_LENGTH - 2 {
            return None;
        }
        let md5 = format!("{fanout}{tail}");
        is_md5(&md5).then_some((md5, directory))
    }
}

fn is_md5(value: &str) -> bool {
    value.len() == MD5_LENGTH && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

/// An out exactly as DVC serializes it, with everything AVC ignores omitted.
///
/// No `deny_unknown_fields`: a `.dvc` file legitimately carries `desc`,
/// `labels`, `type`, `persist`, `remote`, and whatever the next DVC release
/// adds, and none of it changes where the bytes are.
#[derive(Debug, Default, Deserialize)]
struct RawOut {
    path: String,
    #[serde(default)]
    md5: Option<String>,
    /// Very old DVC wrote the digest under this name.
    #[serde(default)]
    checksum: Option<String>,
    /// DVC 3 names the algorithm separately; anything but `md5` is content
    /// this migration cannot address.
    #[serde(default)]
    hash: Option<String>,
    #[serde(default)]
    size: Option<u64>,
    /// `false` means DVC never put the bytes in its cache, so there is nothing
    /// on the remote to migrate.
    #[serde(default = "yes")]
    cache: bool,
    /// Set for an out tracked by cloud versioning rather than by content, which
    /// AVC has no equivalent for.
    #[serde(default)]
    etag: Option<String>,
}

fn yes() -> bool {
    true
}

#[derive(Debug, Default, Deserialize)]
struct RawDvcFile {
    #[serde(default)]
    outs: Vec<RawOut>,
}

#[derive(Debug, Default, Deserialize)]
struct RawStage {
    #[serde(default)]
    outs: Vec<RawOut>,
}

#[derive(Debug, Default, Deserialize)]
struct RawLock {
    #[serde(default)]
    stages: BTreeMap<String, RawStage>,
}

/// What a DVC file yielded, and what it did not.
///
/// Skips are carried rather than logged in place: one migration reads thousands
/// of these files across every commit in a history, and the same unsupported
/// out would otherwise be reported once per commit it appears in.
#[derive(Clone, Debug, Default)]
pub(crate) struct Parsed {
    pub(crate) outs: Vec<Out>,
    pub(crate) skipped: Vec<String>,
}

/// Read a `.dvc` pointer file.
///
/// `directory` is the file's own directory within the repository, because an
/// out's path is written relative to that rather than to the repository root.
pub(crate) fn parse_dvc_file(text: &str, directory: &str) -> Result<Parsed, Failure> {
    let raw: RawDvcFile = serde_yaml::from_str(text)
        .map_err(|error| Failure::from(format!("could not read a .dvc file: {error}")))?;
    Ok(collect(raw.outs, directory))
}

/// Read a `dvc.lock`, which records what a pipeline's stages produced.
///
/// A stage out is tracked data exactly as a `.dvc` file's out is — same cache,
/// same remote, same object — so leaving these behind would migrate a
/// repository whose pipeline outputs no longer resolve.
pub(crate) fn parse_dvc_lock(text: &str, directory: &str) -> Result<Parsed, Failure> {
    let raw: RawLock = serde_yaml::from_str(text)
        .map_err(|error| Failure::from(format!("could not read a dvc.lock: {error}")))?;
    let mut parsed = Parsed::default();
    for stage in raw.stages.into_values() {
        let mut one = collect(stage.outs, directory);
        parsed.outs.append(&mut one.outs);
        parsed.skipped.append(&mut one.skipped);
    }
    Ok(parsed)
}

/// A DVC directory manifest: a JSON array of hashes and relative paths.
///
/// Parsed as YAML, of which JSON is a subset, so reading it costs no extra
/// dependency. Note what is *not* here: sizes. DVC does not record them in a
/// manifest, which is why the migration takes a full listing of the remote and
/// reads the sizes from that.
pub(crate) fn parse_dir_manifest(bytes: &[u8]) -> Result<Vec<DirEntry>, Failure> {
    let text = std::str::from_utf8(bytes)
        .map_err(|_| Failure::from("a DVC directory manifest is not UTF-8"))?;
    let entries: Vec<DirEntry> = serde_yaml::from_str(text)
        .map_err(|error| Failure::from(format!("could not read a DVC .dir manifest: {error}")))?;
    for entry in &entries {
        if !is_md5(&entry.md5) {
            return Err(format!(
                "a DVC directory manifest names {} with a hash that is not an MD5: {}",
                entry.relpath, entry.md5
            )
            .into());
        }
    }
    Ok(entries)
}

/// Turn raw outs into the ones AVC can carry, collecting the rest as skips.
fn collect(outs: Vec<RawOut>, directory: &str) -> Parsed {
    let mut parsed = Parsed::default();
    for out in outs {
        let path = match join_repo_path(directory, &out.path) {
            Some(path) => path,
            None => {
                parsed
                    .skipped
                    .push(format!("{}: path escapes the repository", out.path));
                continue;
            }
        };
        if !out.cache {
            parsed
                .skipped
                .push(format!("{path}: `cache: false`, so DVC stored no object"));
            continue;
        }
        // `hash: md5` is DVC 3 being explicit. Absent means md5 too, since no
        // earlier version wrote anything else. Any other value names an
        // algorithm whose digest is not what the key layout expects.
        if let Some(named) = out.hash.as_deref().filter(|name| *name != "md5") {
            parsed
                .skipped
                .push(format!("{path}: hashed with `{named}`, not md5"));
            continue;
        }
        let Some(digest) = out.md5.or(out.checksum) else {
            let reason = if out.etag.is_some() {
                "tracked by cloud versioning rather than by content"
            } else {
                "no md5 recorded"
            };
            parsed.skipped.push(format!("{path}: {reason}"));
            continue;
        };
        let (digest, directory_out) = match digest.strip_suffix(DIR_SUFFIX) {
            Some(stripped) => (stripped.to_owned(), true),
            None => (digest, false),
        };
        if !is_md5(&digest) {
            parsed
                .skipped
                .push(format!("{path}: `{digest}` is not an md5 digest"));
            continue;
        }
        parsed.outs.push(Out {
            path,
            md5: digest.to_ascii_lowercase(),
            directory: directory_out,
            size: out.size,
        });
    }
    parsed
}

/// Resolve an out's path against the directory of the file that declared it.
///
/// Lexical, and `..` is resolved here rather than rejected, because DVC
/// legitimately writes `../data` in a `.dvc` file inside a subdirectory. What
/// is rejected is a path that climbs past the repository root, or an absolute
/// one — the same rule AVC applies to a pointer, for the same reason.
fn join_repo_path(directory: &str, path: &str) -> Option<String> {
    if Path::new(path).is_absolute() || path.contains('\\') {
        return None;
    }
    let mut segments: Vec<&str> = Vec::new();
    for segment in directory
        .split('/')
        .chain(path.split('/'))
        .filter(|segment| !segment.is_empty() && *segment != ".")
    {
        if segment == ".." {
            segments.pop()?;
            continue;
        }
        segments.push(segment);
    }
    let joined = segments.join("/");
    (!joined.is_empty()).then_some(joined)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_a_file_out_and_a_directory_out() {
        let text = concat!(
            "outs:\n",
            "- md5: 8f2a1c3d4e5f60718293a4b5c6d7e8f9\n  size: 1234\n  path: model.bin\n",
            "- md5: aabbccddeeff00112233445566778899.dir\n  size: 4096\n  nfiles: 12\n  path: data\n",
        );
        let parsed = parse_dvc_file(text, "").unwrap();
        assert_eq!(parsed.skipped, Vec::<String>::new());
        assert_eq!(parsed.outs.len(), 2);
        assert_eq!(parsed.outs[0].path, "model.bin");
        assert!(!parsed.outs[0].directory);
        assert_eq!(parsed.outs[0].size, Some(1234));
        assert!(parsed.outs[1].directory);
        // The `.dir` suffix is DVC's key convention, not part of the digest.
        assert_eq!(parsed.outs[1].md5, "aabbccddeeff00112233445566778899");
    }

    #[test]
    fn an_out_path_is_relative_to_its_dvc_file() {
        let text = "outs:\n- md5: 8f2a1c3d4e5f60718293a4b5c6d7e8f9\n  path: weights.bin\n";
        let parsed = parse_dvc_file(text, "models/bert").unwrap();
        assert_eq!(parsed.outs[0].path, "models/bert/weights.bin");

        // DVC writes `../` freely, and it means what it says.
        let up = "outs:\n- md5: 8f2a1c3d4e5f60718293a4b5c6d7e8f9\n  path: ../shared.bin\n";
        assert_eq!(
            parse_dvc_file(up, "models/bert").unwrap().outs[0].path,
            "models/shared.bin"
        );
        // Past the root is not a path in the repository at all.
        let escape =
            "outs:\n- md5: 8f2a1c3d4e5f60718293a4b5c6d7e8f9\n  path: ../../../etc/passwd\n";
        let parsed = parse_dvc_file(escape, "models").unwrap();
        assert!(parsed.outs.is_empty());
        assert_eq!(parsed.skipped.len(), 1);
    }

    #[test]
    fn unsupported_outs_are_reported_rather_than_guessed_at() {
        let text = "outs:\n\
            - md5: 8f2a1c3d4e5f60718293a4b5c6d7e8f9\n  path: kept.bin\n\
            - path: uncached.bin\n  md5: 8f2a1c3d4e5f60718293a4b5c6d7e8f9\n  cache: false\n\
            - path: sha.bin\n  hash: sha256\n  md5: 8f2a1c3d4e5f60718293a4b5c6d7e8f9\n\
            - path: versioned.bin\n  etag: \"abc\"\n";
        let parsed = parse_dvc_file(text, "").unwrap();
        assert_eq!(parsed.outs.len(), 1, "only the addressable out migrates");
        assert_eq!(parsed.outs[0].path, "kept.bin");
        assert_eq!(parsed.skipped.len(), 3);
        assert!(parsed.skipped[2].contains("cloud versioning"));
    }

    #[test]
    fn reads_pipeline_outputs_out_of_a_lock_file() {
        let text = concat!(
            "schema: '2.0'\n",
            "stages:\n",
            "  train:\n",
            "    cmd: python train.py\n",
            "    deps:\n",
            "    - path: src/train.py\n",
            "      md5: ffffffffffffffffffffffffffffffff\n",
            "    outs:\n",
            "    - path: model.pkl\n",
            "      hash: md5\n",
            "      md5: 8f2a1c3d4e5f60718293a4b5c6d7e8f9\n",
            "      size: 99\n",
        );
        let parsed = parse_dvc_lock(text, "").unwrap();
        // Deps are inputs to a stage, not tracked data of their own.
        assert_eq!(parsed.outs.len(), 1);
        assert_eq!(parsed.outs[0].path, "model.pkl");
        assert_eq!(parsed.outs[0].size, Some(99));
    }

    #[test]
    fn a_dir_manifest_is_json_and_reads_as_yaml() {
        let bytes = br#"[{"md5": "aabbccddeeff00112233445566778899", "relpath": "a/b.bin"},
                         {"md5": "00112233445566778899aabbccddeeff", "relpath": "c.bin"}]"#;
        let entries = parse_dir_manifest(bytes).unwrap();
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[1].relpath, "c.bin");
        // A manifest decides what gets downloaded and where it is written.
        assert!(parse_dir_manifest(br#"[{"md5": "nope", "relpath": "a"}]"#).is_err());
    }

    #[test]
    fn object_keys_round_trip_in_both_layouts() {
        let md5 = "aabbccddeeff00112233445566778899";
        assert_eq!(
            Layout::FilesMd5.key(md5, false),
            "files/md5/aa/bbccddeeff00112233445566778899"
        );
        assert_eq!(
            Layout::Legacy.key(md5, true),
            "aa/bbccddeeff00112233445566778899.dir"
        );
        for layout in [Layout::FilesMd5, Layout::Legacy] {
            for directory in [true, false] {
                let key = layout.key(md5, directory);
                assert_eq!(
                    layout.md5_from_key(&key),
                    Some((md5.to_owned(), directory)),
                    "{layout:?} {directory}"
                );
            }
        }
        // A legacy remote read as a DVC 3 one finds nothing, rather than
        // finding the wrong thing.
        assert_eq!(
            Layout::FilesMd5.md5_from_key("aa/bbccddeeff00112233445566778899"),
            None
        );
        assert_eq!(Layout::Legacy.md5_from_key("some/other/file.txt"), None);
    }
}
