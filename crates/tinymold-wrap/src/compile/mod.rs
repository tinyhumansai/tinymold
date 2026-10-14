//! Serving one cacheable rustc invocation: look up, restore, or run and store.
//!
//! The steps follow `docs/specs/rustc-wrapper.md`:
//!
//! 1. compute the pre-key from the arguments, the compiler, the `--extern`
//!    artifacts, native search directories, a few env vars and the linker;
//! 2. for each input set recorded under that pre-key, newest first, hash the
//!    inputs (reusing digests of unchanged files), compute the full key, and
//!    look for an action under it;
//! 3. on a hit, place every output by reflink or copy, give it a fresh mtime,
//!    replay rustc's output, and record the hit as the last use of its action
//!    and pre-key records (`docs/specs/gc.md`);
//! 4. on a local miss, lock the compile this most likely is, so that other
//!    builds wanting it wait; if another build holds the lock, wait for it
//!    and look again ([`crate::flight`]);
//! 5. with a remote tier, on a local miss, fetch the unit's input sets and
//!    action from it and restore that (`docs/specs/remote.md`);
//! 6. on a miss, run rustc with its output streamed through, then store the
//!    outputs, the action, and the input set, if the compile succeeded and no
//!    input changed while it ran.

use std::collections::{HashMap, HashSet};
use std::ffi::OsString;
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use tinymold_cas::{Store, Strategy};
use tinymold_hash::Digest;
use tinymold_key::{DepInfo, Invocation, KEY_ENV, PreKeyInputs, Roots, full_key, pre_key};

use crate::clock::{filesystem_now, written_before};
use crate::error::io;
use crate::exec::{self, Captured};
use crate::flight::{self, Claim};
use crate::parallel;
use crate::placed::{Placed, Placement};
use crate::record::{
    self, Action, Identity, Input, InputSet, MAX_SETS, Output, decode_sets, encode_sets,
    write_atomically,
};
use crate::remote::{Remote, RemoteError};
use crate::stat;
use crate::trace::Trace;
use crate::{Error, Result};

/// The fewest outputs a hit places on worker threads rather than in turn.
///
/// Most units have two to four outputs, and spawning a thread costs about as
/// much as reflinking a small file, so those stay serial. An incremental
/// compile with split debuginfo leaves dozens to hundreds of object files,
/// and 16 is where every one of up to eight workers still gets a couple of
/// files to amortize its spawn over.
const PARALLEL_RESTORE_MIN: usize = 16;

/// How long a miss waits for another build compiling the same unit before
/// compiling it too.
pub(crate) const LOCK_WAIT: Duration = Duration::from_secs(600);

/// How a cacheable invocation was served. `waited` is how long it waited
/// for another build compiling the same unit, when it did.
#[derive(Debug)]
pub(crate) enum Served {
    /// Restored from the local cache; rustc did not run.
    Hit { waited: Option<Duration> },
    /// Fetched from the remote tier, then restored; rustc did not run.
    RemoteHit { waited: Option<Duration> },
    /// rustc ran and exited with `code`; `stored` says how long storing its
    /// outputs took, or why they were not stored.
    Miss {
        code: i32,
        stored: std::result::Result<Duration, String>,
        /// Why the remote tier could not serve the unit, when it failed.
        remote: Option<String>,
        waited: Option<Duration>,
    },
}

/// What a local lookup found.
struct Lookup {
    /// The matching action and the path of its record.
    hit: Option<(Action, PathBuf)>,
    /// The full key the newest recorded input set gives for today's files:
    /// the compile a miss most likely is.
    likely: Option<Digest>,
}

/// Everything about one invocation the steps share.
struct Unit<'a> {
    store_dir: &'a Path,
    store: Store,
    invocation: &'a Invocation,
    cwd: PathBuf,
    out_dir: PathBuf,
    roots: Roots,
    /// The filesystem clock read before anything was hashed or run: the
    /// moment input sets and stat entries are recorded as of, and the
    /// moment an input must have been written before for the compile to be
    /// stored.
    now_ns: u64,
}

/// Serves `invocation` from the cache in `store_dir`, or by running
/// `program`, waiting up to `lock_wait` for another build compiling the same
/// unit.
///
/// An error means rustc has not run and nothing was restored, so the caller
/// can fall back to running rustc itself.
pub(crate) fn serve(
    store_dir: &Path,
    remote: Option<&dyn Remote>,
    program: &Path,
    args: &[OsString],
    invocation: &Invocation,
    lock_wait: Duration,
    trace: &mut Trace,
) -> Result<Served> {
    let cwd = std::env::current_dir().map_err(io("read", Path::new(".")))?;
    let cargo_home = std::env::var_os("CARGO_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".cargo")));
    let unit = Unit {
        store_dir,
        store: Store::open(store_dir)?,
        invocation,
        out_dir: cwd.join(&invocation.out_dir),
        roots: Roots::new(&cwd, &invocation.out_dir, cargo_home.as_deref()),
        cwd,
        now_ns: filesystem_now(&store_dir.join("stat"))?,
    };
    let pre = trace.span("pre_key", || unit.pre_key(program))?;
    let found = trace.span("lookup", || unit.lookup(&pre))?;
    if unit.restore_hit(&pre, found.hit, trace) {
        return Ok(Served::Hit { waited: None });
    }
    // Held until the compile is stored, so that a build wanting the same
    // unit waits for it rather than compiling it again.
    let lock_key = found.likely.unwrap_or(pre);
    let (_flight, waited) =
        match trace.span("wait", || flight::claim(store_dir, &lock_key, lock_wait)) {
            Claim::Held(flight) => (Some(flight), None),
            Claim::Waited(flight, waited) => {
                let again = trace.span("lookup", || unit.lookup(&pre))?;
                if unit.restore_hit(&pre, again.hit, trace) {
                    return Ok(Served::Hit {
                        waited: Some(waited),
                    });
                }
                (Some(flight), Some(waited))
            }
            Claim::Unlocked => (None, None),
        };
    let mut remote_failure = None;
    if let Some(remote) = remote {
        match trace.span("remote", || unit.remote_lookup(remote, &pre)) {
            Ok(Some((action, entry)))
                if trace.span("restore", || unit.restore(&action)).is_ok() =>
            {
                record::touch(&entry);
                return Ok(Served::RemoteHit { waited });
            }
            Ok(_) => {}
            Err(error) => remote_failure = Some(error.to_string()),
        }
    }

    let captured = trace.span("rustc", || exec::run_captured(program, args))?;
    let stored = if captured.code == 0 {
        let ingesting = std::time::Instant::now();
        trace
            .span("ingest", || unit.ingest(&pre, &captured))
            .map(|()| ingesting.elapsed())
            .map_err(|error| error.to_string())
    } else {
        Err("rustc failed".to_owned())
    };
    Ok(Served::Miss {
        code: captured.code,
        stored,
        remote: remote_failure,
        waited,
    })
}

/// Why a successful compile was not stored.
#[derive(Debug)]
enum Unstorable {
    Error(Error),
    Reason(&'static str),
}

impl std::fmt::Display for Unstorable {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Error(error) => write!(f, "{error}"),
            Self::Reason(reason) => f.write_str(reason),
        }
    }
}

impl<E: Into<Error>> From<E> for Unstorable {
    fn from(error: E) -> Self {
        Self::Error(error.into())
    }
}

impl Unit<'_> {
    fn pre_key(&self, program: &Path) -> Result<Digest> {
        let invocation = self.invocation;
        let rustc_version = stat::rustc_version(self.store_dir, program)?;
        let externs = invocation
            .externs
            .iter()
            .map(|ext| {
                ext.path
                    .as_ref()
                    .map(|path| stat::digest(self.store_dir, &self.cwd.join(path), self.now_ns))
                    .transpose()
            })
            .collect::<Result<Vec<_>>>()?;
        let search_paths = invocation
            .search_paths
            .iter()
            .map(|search| self.listing(&self.cwd.join(&search.path)))
            .collect::<Result<Vec<_>>>()?;
        let env: Vec<(String, Option<String>)> = KEY_ENV
            .iter()
            .map(|name| ((*name).to_owned(), std::env::var(name).ok()))
            .collect();
        let linker = invocation.links().then(|| {
            let linker = invocation.linker.as_deref().unwrap_or(Path::new("cc"));
            stat::linker_identity(linker, &self.cwd)
        });
        Ok(pre_key(&PreKeyInputs {
            invocation,
            roots: &self.roots,
            rustc_version: &rustc_version,
            env: &env,
            externs: &externs,
            search_paths: &search_paths,
            linker: linker.as_deref(),
        }))
    }

    /// The `(name, digest)` of every file in a search directory, sorted; empty
    /// when the directory does not exist.
    fn listing(&self, dir: &Path) -> Result<Vec<(String, Digest)>> {
        let entries = match fs::read_dir(dir) {
            Ok(entries) => entries,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(error) => return Err(io("read", dir)(error)),
        };
        let mut files = Vec::new();
        for entry in entries {
            let path = entry.map_err(io("read", dir))?.path();
            if path.is_file() {
                let name = path
                    .file_name()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .into_owned();
                // Archives a build script rebuilt keep their digest when only
                // their build stamps changed.
                let digest =
                    stat::digest_as(self.store_dir, &path, stat::Kind::Archive, self.now_ns)?;
                files.push((name, digest));
            }
        }
        files.sort();
        Ok(files)
    }

    fn prekey_path(&self, pre: &Digest) -> PathBuf {
        self.store_dir.join("prekeys").join(pre.to_hex())
    }

    fn action_path(&self, key: &Digest) -> PathBuf {
        self.store_dir
            .join("entries")
            .join(&self.invocation.crate_name)
            .join(key.to_hex())
    }

    /// The action of the first recorded input set that still matches, and
    /// the path of its record.
    fn lookup(&self, pre: &Digest) -> Result<Lookup> {
        let mut found = Lookup {
            hit: None,
            likely: None,
        };
        let path = self.prekey_path(pre);
        let Some(text) = record::read(&path)? else {
            return Ok(found);
        };
        for set in decode_sets(&text, &path)? {
            let Some(key) = self.full_key_of(pre, &set) else {
                continue;
            };
            found.likely.get_or_insert(key);
            let entry = self.action_path(&key);
            if let Some(text) = record::read(&entry)? {
                let action = Action::decode(&text, &entry)?;
                found.hit = Some((action, entry));
                break;
            }
        }
        Ok(found)
    }

    /// Restores `hit`, when there is one, and records it as the last use of
    /// its action and of the pre-key. False when there is nothing to restore
    /// or restoring failed, so the unit must be compiled.
    fn restore_hit(&self, pre: &Digest, hit: Option<(Action, PathBuf)>, trace: &mut Trace) -> bool {
        let Some((action, entry)) = hit else {
            return false;
        };
        if trace.span("restore", || self.restore(&action)).is_err() {
            return false;
        }
        record::touch(&entry);
        record::touch(&self.prekey_path(pre));
        true
    }

    /// The first remote input set that matches the local files and whose
    /// action the remote can provide, written into the local cache.
    fn remote_lookup(
        &self,
        remote: &dyn Remote,
        pre: &Digest,
    ) -> std::result::Result<Option<(Action, PathBuf)>, RemoteError> {
        let Some(text) = remote.input_sets(pre)? else {
            return Ok(None);
        };
        let mut sets = decode_sets(&text, &self.prekey_path(pre))?;
        for set in &mut sets {
            // Another machine's inode numbers and mtimes say nothing here:
            // every input is hashed again.
            for input in &mut set.inputs {
                input.identity = Identity::default();
            }
        }
        for set in sets {
            let Some(key) = self.full_key_of(pre, &set) else {
                continue;
            };
            let entry = self.action_path(&key);
            let text = match record::read(&entry)? {
                Some(text) => text,
                None => match remote.fetch_action(&key, &self.store)? {
                    Some(text) => text,
                    None => continue,
                },
            };
            let action = Action::decode(&text, &entry)?;
            if let Some(missing) = action
                .references()
                .into_iter()
                .find(|digest| !self.store.contains(digest))
            {
                return Err(format!("the remote action does not provide object {missing}").into());
            }
            write_atomically(&entry, &text)?;
            self.remember_set(pre, set)?;
            return Ok(Some((action, entry)));
        }
        Ok(None)
    }

    /// The full key `set` gives today, or `None` when one of its inputs is
    /// gone or unreadable.
    fn full_key_of(&self, pre: &Digest, set: &InputSet) -> Option<Digest> {
        let mut inputs = Vec::with_capacity(set.inputs.len());
        for input in &set.inputs {
            let path = self.cwd.join(self.roots.expand(&input.path));
            let current = stat::identity(&path).ok()?;
            let digest = if stat::still_valid(input.identity, set.recorded_ns, current) {
                input.digest
            } else {
                Digest::of_file(&path).ok()?
            };
            inputs.push((input.path.clone(), digest));
        }
        let env: Vec<(String, Option<String>)> = set
            .env
            .iter()
            .map(|name| (name.clone(), std::env::var(name).ok()))
            .collect();
        Some(full_key(pre, &inputs, &env))
    }

    /// Places every output of `action` and replays rustc's output.
    fn restore(&self, action: &Action) -> Result<()> {
        // Read the replayed text first, so a missing object fails the hit
        // before any output is touched.
        let stdout = self.expand_object(&action.stdout)?;
        let stderr = self.expand_object(&action.stderr)?;
        // Outputs the last hit or miss of this unit left here, unchanged
        // since and with the digests this action wants, stay in place.
        let record = self.placed_path();
        let placed = Placed::load(&record)?;
        let wanted: HashMap<&str, Digest> = action
            .outputs
            .iter()
            .filter(|output| !output.text)
            .map(|output| (output.name.as_str(), output.digest))
            .collect();
        // Clear every other file of this unit: a stale `.rmeta` from another
        // branch left beside a restored `.rlib` would have dependents compiled
        // against one version and linked against another.
        let mut kept = HashSet::new();
        for (name, path) in self.unit_files()? {
            let holds = wanted.get(name.as_str()).is_some_and(|digest| {
                fs::symlink_metadata(&path).is_ok_and(|meta| {
                    meta.is_file() && placed.holds(&name, stat::identity_of(&meta), *digest)
                })
            });
            if holds {
                kept.insert(name);
                continue;
            }
            match fs::remove_file(&path) {
                Ok(()) => {}
                Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                Err(error) => return Err(io("replace", &path)(error)),
            }
        }
        let placements = if action.outputs.len() < PARALLEL_RESTORE_MIN {
            action
                .outputs
                .iter()
                .map(|output| self.place(output, kept.contains(&output.name)))
                .collect::<Result<Vec<_>>>()?
        } else {
            parallel::map(&action.outputs, "restoring outputs", |output| {
                self.place(output, kept.contains(&output.name))
            })?
        };
        let placements: Vec<Placement> = placements.into_iter().flatten().collect();
        Placed::save(&record, &placements)?;
        // Like rustc, report errors writing our own output only through the
        // exit status of whoever reads it.
        let _ = io::stdout().lock().write_all(stdout.as_bytes());
        let _ = io::stderr().lock().write_all(stderr.as_bytes());
        Ok(())
    }

    /// Places one output of a hit in the out-dir, freshly modified, unless
    /// it is `in_place` already; returns a binary output as it now is.
    fn place(&self, output: &Output, in_place: bool) -> Result<Option<Placement>> {
        let destination = self.out_dir.join(&output.name);
        if output.text {
            let text = self.expand_object(&output.digest)?;
            fs::write(&destination, text).map_err(io("write", &destination))?;
        } else if !in_place {
            // A reflink or a copy, never a hardlink: a linker may rewrite
            // its output in place, and that must not be the store's inode.
            if self
                .store
                .materialize(&output.digest, &destination, Strategy::Reflink)
                .is_err()
            {
                self.store
                    .materialize(&output.digest, &destination, Strategy::Copy)?;
            }
        }
        set_mode(&destination, output.executable)?;
        // Cargo compares the mtimes of a unit's outputs with its inputs', so
        // every output looks freshly written. Objects kept in place are the
        // exception: rustc itself leaves the objects it reuses with their old
        // mtimes, and nothing compares them.
        if !(in_place && is_object(&output.name)) {
            let file = fs::OpenOptions::new()
                .write(true)
                .open(&destination)
                .map_err(io("open", &destination))?;
            file.set_modified(SystemTime::now())
                .map_err(io("set the mtime of", &destination))?;
        }
        if output.text {
            return Ok(None);
        }
        let identity = stat::identity(&destination)?;
        if is_library(&output.name) {
            // Dependents hash this file next; they need not read it.
            let _ = stat::remember(self.store_dir, &destination, identity, output.digest);
        }
        Ok(Some((output.name.clone(), identity, output.digest)))
    }

    /// Where this unit records the outputs it left in its out-dir.
    fn placed_path(&self) -> PathBuf {
        let [_, stem] = self.invocation.output_prefixes();
        Placed::path(self.store_dir, &self.out_dir, &stem)
    }

    fn expand_object(&self, digest: &Digest) -> Result<String> {
        let bytes = self.store.read(digest)?;
        Ok(self.roots.expand(&String::from_utf8_lossy(&bytes)))
    }

    /// Stores what a successful compile produced.
    fn ingest(&self, pre: &Digest, captured: &Captured) -> std::result::Result<(), Unstorable> {
        let invocation = self.invocation;
        let stem = format!("{}{}", invocation.crate_name, invocation.extra_filename);
        let dep_info = self.out_dir.join(format!("{stem}.d"));
        let Some(text) = record::read(&dep_info)? else {
            return Err(Unstorable::Reason("no dep-info"));
        };
        let info = DepInfo::parse(&text)?;

        let mut inputs = Vec::with_capacity(info.inputs.len());
        for written in &info.inputs {
            let path = self.cwd.join(written);
            let identity = stat::identity(&path)?;
            // Inputs are hashed after the compile, so one written since
            // the unit's clock reading, or within a tick before it, may not
            // be what rustc read.
            if !written_before(identity.mtime_ns, self.now_ns) {
                return Err(Unstorable::Reason("an input changed during the compile"));
            }
            inputs.push(Input {
                path: self.roots.normalize(written),
                identity,
                digest: Digest::of_file(&path)?,
            });
        }
        let key_inputs: Vec<(String, Digest)> = inputs
            .iter()
            .map(|input| (input.path.clone(), input.digest))
            .collect();
        let key = full_key(pre, &key_inputs, &info.env);

        let (outputs, placements) = self.ingest_outputs()?;
        let action = Action {
            stdout: self.store_text(&captured.stdout)?,
            stderr: self.store_text(&captured.stderr)?,
            outputs,
        };
        write_atomically(&self.action_path(&key), &action.encode())?;
        Placed::save(&self.placed_path(), &placements)?;

        let set = InputSet {
            recorded_ns: self.now_ns,
            inputs,
            env: info.env.into_iter().map(|(name, _)| name).collect(),
        };
        self.remember_set(pre, set)?;
        Ok(())
    }

    /// Stores every output this compile wrote and describes them, with the
    /// binary outputs as they are on disk.
    fn ingest_outputs(&self) -> std::result::Result<(Vec<Output>, Vec<Placement>), Unstorable> {
        // Every file with this unit's prefix, whatever its mtime: incremental
        // rustc hardlinks artifacts it reuses (objects, sometimes the .rmeta)
        // back into the out-dir with their old mtimes, and they are as much
        // this compile's outputs as the files it wrote. Files left from an
        // earlier compile of the same unit only add bytes, never wrong ones,
        // since rustc rewrites every output it emits.
        let mut found = Vec::new();
        for (name, path) in self.unit_files()? {
            let meta = fs::symlink_metadata(&path).map_err(io("read", &path))?;
            if !meta.is_file() {
                return Err(Unstorable::Reason("an output is not a regular file"));
            }
            found.push((name, path, meta));
        }
        for required in self.required_outputs() {
            if !found.iter().any(|(name, _, _)| *name == required) {
                return Err(Unstorable::Reason("an expected output is missing"));
            }
        }
        if found.is_empty() {
            return Err(Unstorable::Reason("no outputs found"));
        }

        // An incremental compile with split debuginfo writes hundreds of
        // object files; storing them one by one would hold cargo up for
        // longer than the compile's own link.
        let placed = Placed::load(&self.placed_path()).unwrap_or_default();
        let mut outputs = parallel::map(&found, "storing outputs", |file| {
            self.store_output(file, &placed)
        })?;
        outputs.sort_by(|a, b| a.name.cmp(&b.name));
        let identities: HashMap<&str, Identity> = found
            .iter()
            .map(|(name, _, meta)| (name.as_str(), stat::identity_of(meta)))
            .collect();
        let placements = outputs
            .iter()
            .filter(|output| !output.text)
            .filter_map(|output| {
                let identity = identities.get(output.name.as_str())?;
                Some((output.name.clone(), *identity, output.digest))
            })
            .collect();
        Ok((outputs, placements))
    }

    /// Stores one output file, unless `placed` shows it is one the store
    /// already holds and rustc left untouched.
    fn store_output(
        &self,
        (name, path, meta): &(String, PathBuf, fs::Metadata),
        placed: &Placed,
    ) -> std::result::Result<Output, Unstorable> {
        let extension = Path::new(name).extension().and_then(|ext| ext.to_str());
        let text = extension == Some("d");
        let digest = if text {
            let content = fs::read(path).map_err(io("read", path))?;
            self.store_text(&content)?
        } else if let Some(digest) = self.carried_over(name, meta, placed) {
            digest
        } else {
            let digest = self.store.insert_file(path)?;
            if is_library(name) {
                // A dependent hashes this file next; it need not read it.
                let _ = stat::remember(self.store_dir, path, stat::identity_of(meta), digest);
            }
            digest
        };
        Ok(Output {
            name: name.clone(),
            digest,
            executable: is_executable(meta),
            text,
        })
    }

    /// The digest of an output this compile did not write: one the last hit
    /// or miss placed, with the recorded identity, an mtime from before the
    /// compile's clock reading, and its object still in the store. Incremental
    /// rustc carries most objects over this way, and hashing them again
    /// would cost an edit more than compiling the ones that changed.
    fn carried_over(&self, name: &str, meta: &fs::Metadata, placed: &Placed) -> Option<Digest> {
        carried_over(
            &self.store,
            placed,
            name,
            stat::identity_of(meta),
            self.now_ns,
        )
    }

    /// Every entry of the out-dir whose name starts with one of this unit's
    /// output prefixes.
    fn unit_files(&self) -> Result<Vec<(String, PathBuf)>> {
        let prefixes = self.invocation.output_prefixes();
        let entries = match fs::read_dir(&self.out_dir) {
            Ok(entries) => entries,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(error) => return Err(io("read", &self.out_dir)(error)),
        };
        let mut files = Vec::new();
        for entry in entries {
            let entry = entry.map_err(io("read", &self.out_dir))?;
            if let Some(name) = entry.file_name().to_str()
                && prefixes
                    .iter()
                    .any(|prefix| name.starts_with(prefix.as_str()))
            {
                files.push((name.to_owned(), entry.path()));
            }
        }
        Ok(files)
    }

    /// Outputs a successful compile must have written, by `--emit` kind: the
    /// dep-info, the `.rmeta`, and the `.rlib` of a library.
    fn required_outputs(&self) -> Vec<String> {
        let invocation = self.invocation;
        let [lib_stem, stem] = invocation.output_prefixes();
        let emits = |kind: &str| invocation.emit.iter().any(|emit| emit == kind);
        let mut required = Vec::new();
        if emits("dep-info") {
            required.push(format!("{stem}.d"));
        }
        if emits("metadata") {
            required.push(format!("{lib_stem}.rmeta"));
        }
        let library = invocation
            .crate_types
            .iter()
            .any(|kind| kind == "lib" || kind == "rlib");
        if emits("link") && library {
            required.push(format!("{lib_stem}.rlib"));
        }
        required
    }

    /// Stores `bytes` as text with this unit's roots replaced by placeholders.
    fn store_text(&self, bytes: &[u8]) -> std::result::Result<Digest, Unstorable> {
        let text =
            std::str::from_utf8(bytes).map_err(|_| Unstorable::Reason("output is not UTF-8"))?;
        Ok(self
            .store
            .insert_bytes(self.roots.normalize(text).as_bytes())?)
    }

    /// Adds `set` as the newest input set of `pre`, replacing an older set
    /// that read the same files and variables.
    fn remember_set(&self, pre: &Digest, set: InputSet) -> Result<()> {
        let path = self.prekey_path(pre);
        let mut sets = record::read(&path)?
            .and_then(|text| decode_sets(&text, &path).ok())
            .unwrap_or_default();
        let same_shape = |other: &InputSet| {
            other.env == set.env
                && other.inputs.len() == set.inputs.len()
                && other
                    .inputs
                    .iter()
                    .zip(&set.inputs)
                    .all(|(a, b)| a.path == b.path)
        };
        sets.retain(|other| !same_shape(other));
        sets.insert(0, set);
        sets.truncate(MAX_SETS);
        write_atomically(&path, &encode_sets(&sets))
    }
}

/// Whether dependents read the output `name` as an `--extern`.
fn is_library(name: &str) -> bool {
    matches!(
        Path::new(name).extension().and_then(|ext| ext.to_str()),
        Some("rlib" | "rmeta" | "dylib" | "so" | "dll")
    )
}

/// Whether `name` is an object file.
fn is_object(name: &str) -> bool {
    Path::new(name).extension().is_some_and(|ext| ext == "o")
}

#[cfg(unix)]
fn is_executable(meta: &fs::Metadata) -> bool {
    use std::os::unix::fs::PermissionsExt;
    meta.permissions().mode() & 0o111 != 0
}

#[cfg(not(unix))]
fn is_executable(_meta: &fs::Metadata) -> bool {
    false
}

#[cfg(unix)]
fn set_mode(path: &Path, executable: bool) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let mode = if executable { 0o755 } else { 0o644 };
    fs::set_permissions(path, fs::Permissions::from_mode(mode)).map_err(io("set the mode of", path))
}

#[cfg(not(unix))]
fn set_mode(path: &Path, _executable: bool) -> Result<()> {
    let mut permissions = fs::metadata(path).map_err(io("read", path))?.permissions();
    #[allow(clippy::permissions_set_readonly_false)]
    permissions.set_readonly(false);
    fs::set_permissions(path, permissions).map_err(io("set the mode of", path))
}

#[cfg(test)]
mod test;
