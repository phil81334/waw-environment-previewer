//! Private, versioned compiled CPU snapshots. Never distributed game assets.
//! Commit records are immutable and published last; a failed save cannot replace
//! the previous complete snapshot. GPU resources/probe captures are recreated.
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, HashMap},
    fs,
    io::{Read, Write},
    path::{Component, Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
    time::{SystemTime, UNIX_EPOCH},
};
use waw_environment_previewer::{
    level_fx::{self, LevelFxDocument},
    validate_probes, BakedScene, ProbeSettings, SceneManifest,
};

const SCHEMA: u32 = 2;
const MAX_INDEX: usize = 64 * 1024 * 1024;
const MAX_BUFFER: usize = 256 * 1024 * 1024;
const MAX_TOTAL: usize = 1024 * 1024 * 1024;
const MAX_BUFFERS: usize = 8192;
const RETAIN_SNAPSHOTS: usize = 6;
static NEXT: AtomicU64 = AtomicU64::new(1);

#[derive(Serialize, Deserialize)]
struct Buffer {
    key: String,
    bytes: usize,
    sha256: String,
}
#[derive(Serialize, Deserialize)]
struct Snapshot {
    schema: u32,
    version: String,
    game: String,
    game_exe: String,
    settings: serde_json::Value,
    prefab_hashes: BTreeMap<String, String>,
    effects: LevelFxDocument,
    manifest: SceneManifest,
    buffers: Vec<Buffer>,
}

fn digest(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}
fn regular(path: &Path) -> Result<fs::Metadata, String> {
    let meta = fs::symlink_metadata(path).map_err(|e| e.to_string())?;
    if meta.file_type().is_symlink() || reparse(&meta) || !meta.is_file() {
        return Err("Scene cache requires regular files, without links or reparse points.".into());
    }
    Ok(meta)
}
#[cfg(windows)]
fn reparse(meta: &fs::Metadata) -> bool {
    use std::os::windows::fs::MetadataExt;
    meta.file_attributes() & 0x400 != 0
}
#[cfg(not(windows))]
fn reparse(_: &fs::Metadata) -> bool {
    false
}
fn folder(path: &Path) -> Result<PathBuf, String> {
    fs::create_dir_all(path).map_err(|e| e.to_string())?;
    let meta = fs::symlink_metadata(path).map_err(|e| e.to_string())?;
    if !meta.is_dir() || meta.file_type().is_symlink() || reparse(&meta) {
        return Err("Scene cache directory must not be a link or reparse point.".into());
    }
    path.canonicalize().map_err(|e| e.to_string())
}
fn read(path: &Path, parent: &Path, limit: usize) -> Result<Vec<u8>, String> {
    if regular(path)?.len() > limit as u64 {
        return Err("Scene cache file exceeds its limit.".into());
    }
    let resolved = path.canonicalize().map_err(|e| e.to_string())?;
    if resolved.parent() != Some(parent) {
        return Err("Scene cache file escaped its directory.".into());
    }
    let mut bytes = Vec::new();
    fs::File::open(resolved)
        .map_err(|e| e.to_string())?
        .take(limit as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| e.to_string())?;
    if bytes.len() > limit {
        return Err("Scene cache file exceeds its limit.".into());
    }
    Ok(bytes)
}
fn write(path: &Path, bytes: &[u8]) -> Result<(), String> {
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .map_err(|e| e.to_string())?;
    file.write_all(bytes).map_err(|e| e.to_string())?;
    file.sync_all().map_err(|e| e.to_string())
}
fn game_identity(game: &Path) -> Result<(PathBuf, String), String> {
    let base = game.canonicalize().map_err(|e| e.to_string())?;
    let exe = read(&base.join("CoDWaW.exe"), &base, 128 * 1024 * 1024)?;
    Ok((base, digest(&exe)))
}
// Compare with the bytes the compiler actually received, including nested
// prefabs. An edit during or after baking must never validate an older result.
fn verify_prefabs(game: &Path, hashes: &BTreeMap<String, String>) -> Result<(), String> {
    if hashes.is_empty() {
        return Ok(());
    }
    if hashes.len() > 64 {
        return Err("Too many prefab sources in saved scene.".into());
    }
    let root = game
        .join("map_source")
        .canonicalize()
        .map_err(|error| error.to_string())?;
    if !root.starts_with(game) {
        return Err("Prefab root is outside the installed game.".into());
    }
    for (name, expected) in hashes {
        let relative = Path::new(name);
        if !name.ends_with(".map")
            || relative
                .components()
                .any(|part| !matches!(part, Component::Normal(_)))
        {
            return Err("Invalid prefab reference in saved scene.".into());
        }
        let path = root
            .join(relative)
            .canonicalize()
            .map_err(|error| format!("Cannot read prefab {name}: {error}"))?;
        if !path.starts_with(&root) {
            return Err("Prefab source is outside map_source.".into());
        }
        let bytes = read(
            &path,
            path.parent().ok_or("Invalid prefab path")?,
            16 * 1024 * 1024,
        )?;
        if digest(&bytes) != *expected {
            return Err(format!("Prefab {name} changed since the scene was baked."));
        }
    }
    Ok(())
}

fn equivalent<T: Serialize>(a: &T, b: &T) -> Result<bool, String> {
    Ok(serde_json::to_value(a).map_err(|e| e.to_string())?
        == serde_json::to_value(b).map_err(|e| e.to_string())?)
}

// Shared by the actual loader and focused corruption tests. The paths come only
// from ordinal indices; serialized logical buffer keys never become disk paths.
fn verified_buffers(dir: &Path, records: &[Buffer]) -> Result<HashMap<String, Vec<u8>>, String> {
    if records.len() > MAX_BUFFERS {
        return Err("Too many cached buffers.".into());
    }
    let mut buffers = HashMap::new();
    let mut total = 0usize;
    for (index, record) in records.iter().enumerate() {
        total = total
            .checked_add(record.bytes)
            .ok_or("Cache size overflow.")?;
        if record.bytes > MAX_BUFFER
            || total > MAX_TOTAL
            || record.key.len() > 256
            || buffers.contains_key(&record.key)
        {
            return Err("Invalid scene cache buffer index.".into());
        }
        let bytes = read(
            &dir.join(format!("buffer-{index:05}.bin")),
            dir,
            record.bytes,
        )?;
        if bytes.len() != record.bytes || digest(&bytes) != record.sha256 {
            return Err("Scene cache buffer failed verification.".into());
        }
        buffers.insert(record.key.clone(), bytes);
    }
    Ok(buffers)
}
// Snapshot names sort chronologically. Only the writer creates names with
// this exact layout, and an in-progress save has no current.json.
fn snapshot_name(name: &str) -> bool {
    let Some(rest) = name.strip_prefix("snapshot-") else {
        return false;
    };
    let mut fields = rest.split('-');
    [39, 10, 10].iter().all(|&width| {
        fields.next().is_some_and(|field| {
            field.len() == width && field.bytes().all(|byte| byte.is_ascii_digit())
        })
    }) && fields.next().is_none()
}

fn completed_snapshots(root: &Path) -> Result<Vec<PathBuf>, String> {
    let mut snapshots = Vec::new();
    for entry in fs::read_dir(root).map_err(|e| e.to_string())? {
        let entry = entry.map_err(|e| e.to_string())?;
        if !snapshot_name(&entry.file_name().to_string_lossy()) {
            continue;
        }
        let path = entry.path();
        let Ok(meta) = fs::symlink_metadata(&path) else {
            continue;
        };
        if meta.is_dir()
            && !reparse(&meta)
            && !meta.file_type().is_symlink()
            && regular(&path.join("current.json")).is_ok()
        {
            snapshots.push(path);
        }
    }
    snapshots.sort_unstable_by(|left, right| right.cmp(left));
    Ok(snapshots)
}

// Delete only the ordinary files the cache owns, never recurse or follow links.
fn remove_snapshot(root: &Path, path: &Path) {
    if path.parent() != Some(root) || path.canonicalize().ok().as_deref() != Some(path) {
        return;
    }
    let Ok(entries) = fs::read_dir(path) else {
        return;
    };
    let mut files = Vec::new();
    for entry in entries {
        let Ok(entry) = entry else {
            return;
        };
        let name = entry.file_name();
        let name = name.to_string_lossy();
        let is_buffer = name
            .strip_prefix("buffer-")
            .and_then(|value| value.strip_suffix(".bin"))
            .is_some_and(|index| {
                index.len() == 5 && index.bytes().all(|byte| byte.is_ascii_digit())
            });
        if !(name == "current.json" || name == "pending.json" || is_buffer)
            || regular(&entry.path()).is_err()
        {
            return;
        }
        files.push(entry.path());
    }
    for file in files {
        let _ = fs::remove_file(file);
    }
    let _ = fs::remove_dir(path);
}

struct PendingSnapshot<'a> {
    root: &'a Path,
    output: &'a Path,
    committed: bool,
}

impl Drop for PendingSnapshot<'_> {
    fn drop(&mut self) {
        if !self.committed {
            remove_snapshot(self.root, self.output);
        }
    }
}

pub fn save(
    root: &Path,
    game: &Path,
    scene: &BakedScene,
    settings: &ProbeSettings,
    effects: &LevelFxDocument,
) -> Result<(), String> {
    validate_probes(settings)?;
    let (game, game_exe) = game_identity(game)?;
    verify_prefabs(&game, &scene.prefab_hashes)?;
    let discovered = level_fx::discover()?;
    if !equivalent(effects, &discovered)? {
        return Err("Scene FX sources changed; snapshot was not cached.".into());
    }
    let root = folder(root)?;
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|e| e.to_string())?
        .as_nanos();
    let name = format!(
        "snapshot-{stamp:039}-{:010}-{:010}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    );
    let output = root.join(name);
    fs::create_dir(&output).map_err(|e| e.to_string())?;
    let output = folder(&output)?;
    if output.parent() != Some(root.as_path()) {
        return Err("Scene cache directory escaped its root.".into());
    }
    let mut pending = PendingSnapshot {
        root: &root,
        output: &output,
        committed: false,
    };
    if scene.buffers.len() > MAX_BUFFERS {
        return Err("Too many scene cache buffers.".into());
    }
    let mut keys: Vec<_> = scene.buffers.keys().collect();
    keys.sort();
    let mut buffers = Vec::new();
    let mut total = 0usize;
    for (index, key) in keys.into_iter().enumerate() {
        let bytes = &scene.buffers[key];
        total = total
            .checked_add(bytes.len())
            .ok_or("Scene cache size overflow.")?;
        if bytes.len() > MAX_BUFFER || total > MAX_TOTAL {
            return Err("Scene cache exceeds its size limit.".into());
        }
        write(&output.join(format!("buffer-{index:05}.bin")), bytes)?;
        buffers.push(Buffer {
            key: key.clone(),
            bytes: bytes.len(),
            sha256: digest(bytes),
        });
    }
    let snapshot = Snapshot {
        schema: SCHEMA,
        version: env!("CARGO_PKG_VERSION").into(),
        game: game.to_string_lossy().into_owned(),
        game_exe,
        settings: serde_json::to_value(settings).map_err(|e| e.to_string())?,
        prefab_hashes: scene.prefab_hashes.clone(),
        effects: discovered,
        manifest: scene.manifest.clone(),
        buffers,
    };
    let bytes = serde_json::to_vec(&snapshot).map_err(|e| e.to_string())?;
    if bytes.len() > MAX_INDEX {
        return Err("Scene cache index exceeds its limit.".into());
    }
    verify_prefabs(&game, &snapshot.prefab_hashes)?;
    write(&output.join("pending.json"), &bytes)?;
    fs::rename(output.join("pending.json"), output.join("current.json"))
        .map_err(|e| e.to_string())?;
    pending.committed = true;
    if let Ok(snapshots) = completed_snapshots(&root) {
        for old in snapshots.iter().skip(RETAIN_SNAPSHOTS) {
            remove_snapshot(&root, old);
        }
    }
    Ok(())
}

pub fn load(
    root: &Path,
    game: &Path,
    settings: &ProbeSettings,
) -> Result<Option<(BakedScene, LevelFxDocument)>, String> {
    validate_probes(settings)?;
    if !root.try_exists().map_err(|e| e.to_string())? {
        return Ok(None);
    }
    let root = folder(root)?;
    let (game, game_exe) = game_identity(game)?;
    let expected = serde_json::to_value(settings).map_err(|e| e.to_string())?;
    let current_fx = level_fx::discover()?;
    // Older releases may have left more than the retention limit.
    for path in completed_snapshots(&root)?.into_iter().take(32) {
        let attempt = (|| -> Result<Option<(BakedScene, LevelFxDocument)>, String> {
            let meta = fs::symlink_metadata(&path).map_err(|e| e.to_string())?;
            if !meta.is_dir() || meta.file_type().is_symlink() || reparse(&meta) {
                return Ok(None);
            }
            let dir = path.canonicalize().map_err(|e| e.to_string())?;
            if dir.parent() != Some(root.as_path()) {
                return Err("Scene cache escaped its root.".into());
            }
            let snapshot: Snapshot =
                serde_json::from_slice(&read(&dir.join("current.json"), &dir, MAX_INDEX)?)
                    .map_err(|e| e.to_string())?;
            if snapshot.schema != SCHEMA
                || snapshot.version != env!("CARGO_PKG_VERSION")
                || snapshot.game != game.to_string_lossy()
                || snapshot.game_exe != game_exe
                || snapshot.settings != expected
                || !equivalent(&snapshot.effects, &current_fx)?
            {
                return Ok(None);
            }
            let buffers = verified_buffers(&dir, &snapshot.buffers)?;
            verify_prefabs(&game, &snapshot.prefab_hashes)?;
            Ok(Some((
                BakedScene {
                    manifest: snapshot.manifest,
                    prefab_hashes: snapshot.prefab_hashes,
                    buffers,
                },
                snapshot.effects,
            )))
        })();
        // A corrupt, incomplete or stale record is a cache miss, not a startup failure.
        if let Ok(Some(value)) = attempt {
            return Ok(Some(value));
        }
    }
    Ok(None)
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Fixture(PathBuf);
    impl Fixture {
        fn new() -> Self {
            let serial = NEXT.fetch_add(1, Ordering::Relaxed);
            let stamp = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos();
            let path = std::env::temp_dir().join(format!(
                "waw-env-cache-test-{}-{stamp}-{serial}",
                std::process::id()
            ));
            fs::create_dir(&path).unwrap();
            Self(path.canonicalize().unwrap())
        }
        fn buffer(&self, ordinal: usize, data: &[u8]) {
            fs::write(self.0.join(format!("buffer-{ordinal:05}.bin")), data).unwrap();
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            // Remove only this test's known regular files, never recurse.
            for ordinal in 0..3 {
                let _ = fs::remove_file(self.0.join(format!("buffer-{ordinal:05}.bin")));
            }
            let _ = fs::remove_dir(&self.0);
        }
    }
    fn record(key: &str, data: &[u8]) -> Buffer {
        Buffer {
            key: key.into(),
            bytes: data.len(),
            sha256: digest(data),
        }
    }
    #[test]
    fn cleanup_keeps_pending_and_unrelated_files() {
        let root = Fixture::new();
        let snapshot = |index| {
            root.0
                .join(format!("snapshot-{index:039}-0000000001-0000000001"))
        };
        let old = snapshot(1);
        let new = snapshot(2);
        let unfinished = snapshot(3);
        let unrelated = root.0.join("snapshot-backup");
        for folder in [&old, &new, &unfinished, &unrelated] {
            fs::create_dir(folder).unwrap();
        }
        for folder in [&old, &new, &unrelated] {
            fs::write(folder.join("current.json"), b"{}").unwrap();
        }
        fs::write(unfinished.join("buffer-00000.bin"), b"partial").unwrap();
        assert_eq!(
            completed_snapshots(&root.0).unwrap(),
            [new.clone(), old.clone()]
        );

        fs::write(old.join("notes.txt"), b"keep").unwrap();
        remove_snapshot(&root.0, &old);
        assert!(old.join("current.json").is_file());
        fs::remove_file(old.join("notes.txt")).unwrap();
        remove_snapshot(&root.0, &old);
        assert!(!old.exists());
        let pending = PendingSnapshot {
            root: &root.0,
            output: &unfinished,
            committed: false,
        };
        drop(pending);
        assert!(!unfinished.exists());
        remove_snapshot(&root.0, &new);
        fs::remove_file(unrelated.join("current.json")).unwrap();
        fs::remove_dir(unrelated).unwrap();
    }
    #[test]
    fn verified_buffers_round_trip_and_reject_same_size_corruption() {
        let fixture = Fixture::new();
        let original = b"compiled vertex bytes";
        fixture.buffer(0, original);
        let entries = vec![record("geometry", original)];
        let restored = verified_buffers(&fixture.0, &entries).unwrap();
        assert_eq!(restored["geometry"], original);
        let mut corrupt = original.to_vec();
        corrupt[0] ^= 1;
        fixture.buffer(0, &corrupt);
        assert!(verified_buffers(&fixture.0, &entries)
            .unwrap_err()
            .contains("failed verification"));
    }
    #[test]
    fn verified_buffers_reject_truncation_and_extra_bytes() {
        let fixture = Fixture::new();
        let original = b"compiled index bytes";
        let entries = vec![record("indices", original)];
        fixture.buffer(0, &original[..original.len() - 1]);
        assert!(verified_buffers(&fixture.0, &entries).is_err());
        let mut extended = original.to_vec();
        extended.push(0);
        fixture.buffer(0, &extended);
        assert!(verified_buffers(&fixture.0, &entries).is_err());
    }
    #[test]
    fn verified_buffers_reject_missing_duplicates_and_oversized_index() {
        let fixture = Fixture::new();
        let bytes = b"mesh";
        assert!(verified_buffers(&fixture.0, &[record("geometry", bytes)]).is_err());
        fixture.buffer(0, bytes);
        fixture.buffer(1, bytes);
        assert!(verified_buffers(
            &fixture.0,
            &[record("geometry", bytes), record("geometry", bytes)]
        )
        .is_err());
        let oversized = Buffer {
            key: "geometry".into(),
            bytes: MAX_BUFFER + 1,
            sha256: digest(bytes),
        };
        assert!(verified_buffers(&fixture.0, &[oversized]).is_err());
    }
    #[test]
    fn staged_prefab_hashes_reject_edits_and_removed_sources() {
        let game = Fixture::new();
        let source = game.0.join("map_source");
        fs::create_dir(&source).unwrap();
        let prefab = source.join("small.map");
        fs::write(&prefab, b"used by compiler").unwrap();
        let hashes = BTreeMap::from([("small.map".to_string(), digest(b"used by compiler"))]);
        assert!(verify_prefabs(&game.0, &hashes).is_ok());
        fs::write(&prefab, b"edited after baking").unwrap();
        assert!(verify_prefabs(&game.0, &hashes).is_err());
        fs::remove_file(prefab).unwrap();
        assert!(verify_prefabs(&game.0, &hashes).is_err());
        fs::remove_dir(source).unwrap();
        assert!(verify_prefabs(&game.0, &BTreeMap::new()).is_ok());
    }
}
