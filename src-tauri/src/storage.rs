use serde::Serialize;
use serde_json::Value;
use std::{
    fs::{self, OpenOptions},
    io::{Read, Write},
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};

const MAX_BYTES: usize = 1024 * 1024;
static NEXT_WRITE: AtomicU64 = AtomicU64::new(1);

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct VisionSource {
    name: String,
    text: String,
    default_text: String,
    source: &'static str,
}

fn valid_name(name: &str) -> Result<(), String> {
    if name.is_empty()
        || name.len() > 96
        || !name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"_-".contains(&byte))
    {
        return Err(
            "Use a preset name containing 1–96 letters, digits, underscores or hyphens.".into(),
        );
    }
    let upper = name.to_ascii_uppercase();
    let reserved = ["CON", "PRN", "AUX", "NUL", "CLOCK$"];
    if reserved.contains(&upper.as_str())
        || (upper.len() == 4
            && (upper.starts_with("COM") || upper.starts_with("LPT"))
            && upper.as_bytes()[3].is_ascii_digit())
    {
        return Err("This preset name is reserved by Windows.".into());
    }
    Ok(())
}

fn validate_text(text: &str) -> Result<(), String> {
    if text.len() > MAX_BYTES || !text.is_ascii() || text.as_bytes().contains(&0) {
        return Err(
            "Vision text must be ASCII without NUL characters and no larger than 1 MiB.".into(),
        );
    }
    Ok(())
}

fn contained_file(folder: &Path, name: &str) -> Result<PathBuf, String> {
    valid_name(name)?;
    let base =
        fs::canonicalize(folder).map_err(|error| format!("Cannot open preset folder: {error}"))?;
    let path = base.join(format!("{name}.vision"));
    let resolved =
        fs::canonicalize(&path).map_err(|error| format!("Cannot open preset: {error}"))?;
    if resolved.parent() != Some(base.as_path()) || !resolved.starts_with(&base) {
        return Err("The preset resolves outside its allowed folder.".into());
    }
    Ok(resolved)
}

fn read_limited(path: &Path) -> Result<Vec<u8>, String> {
    let mut file = fs::File::open(path).map_err(|error| format!("Cannot read file: {error}"))?;
    if file.metadata().map_err(|error| error.to_string())?.len() > MAX_BYTES as u64 {
        return Err("This file exceeds the 1 MiB limit.".into());
    }
    let mut bytes = Vec::new();
    Read::by_ref(&mut file)
        .take((MAX_BYTES + 1) as u64)
        .read_to_end(&mut bytes)
        .map_err(|error| error.to_string())?;
    if bytes.len() > MAX_BYTES {
        return Err("This file exceeds the 1 MiB limit.".into());
    }
    Ok(bytes)
}

fn read_vision(folder: &Path, name: &str) -> Result<String, String> {
    let path = contained_file(folder, name)?;
    let bytes = read_limited(&path)?;
    let text = String::from_utf8(bytes).map_err(|_| "Vision text is not ASCII.".to_owned())?;
    validate_text(&text)?;
    Ok(text)
}

pub fn list_visions(folder: &Path) -> Result<Vec<String>, String> {
    if !folder.exists() {
        return Ok(Vec::new());
    }
    let base = fs::canonicalize(folder).map_err(|error| error.to_string())?;
    let mut names = Vec::new();
    for entry in fs::read_dir(&base).map_err(|error| error.to_string())? {
        let entry = entry.map_err(|error| error.to_string())?;
        let path = entry.path();
        if !path
            .extension()
            .and_then(|part| part.to_str())
            .is_some_and(|part| part.eq_ignore_ascii_case("vision"))
        {
            continue;
        }
        let Some(name) = path.file_stem().and_then(|part| part.to_str()) else {
            continue;
        };
        if valid_name(name).is_err() {
            continue;
        }
        let resolved = fs::canonicalize(&path).map_err(|error| error.to_string())?;
        if resolved.parent() == Some(base.as_path()) && resolved.is_file() {
            names.push(name.to_owned());
        }
    }
    names.sort_by_key(|name| name.to_ascii_lowercase());
    Ok(names)
}

pub fn load_vision(game: &Path, name: &str) -> Result<VisionSource, String> {
    let installed = game.join("raw/vision");
    let text = read_vision(&installed, name)?;
    let default_text = if name.eq_ignore_ascii_case("default") {
        text.clone()
    } else {
        read_vision(&installed, "default")?
    };
    Ok(VisionSource {
        name: name.to_owned(),
        text,
        default_text,
        source: "installed",
    })
}

fn private_folder(root: &Path, relative: &[&str]) -> Result<PathBuf, String> {
    fs::create_dir_all(root).map_err(|error| format!("Cannot create app data folder: {error}"))?;
    let base = fs::canonicalize(root).map_err(|error| error.to_string())?;
    let mut target = base.clone();
    for component in relative {
        target.push(component);
        fs::create_dir_all(&target).map_err(|error| error.to_string())?;
        let resolved = fs::canonicalize(&target).map_err(|error| error.to_string())?;
        if !resolved.starts_with(&base) || resolved.parent() != target.parent() {
            return Err("An app output folder resolves outside its private directory.".into());
        }
        target = resolved;
    }
    Ok(target)
}

// A packaged desktop host can virtualize individual AppData file opens even
// when canonicalizing the containing directory returns its logical location.
// Resolve the OS-selected private directory through an owned create_new file
// once, then retain the ordinary containment guards for all later operations.
pub fn resolve_data_root(root: &Path) -> Result<PathBuf, String> {
    fs::create_dir_all(root).map_err(|error| format!("Cannot create app data folder: {error}"))?;
    let counter = NEXT_WRITE.fetch_add(1, Ordering::Relaxed);
    let marker = root.join(format!(
        "entities-location-{}-{counter}.tmp",
        std::process::id()
    ));
    let file = OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(&marker)
        .map_err(|error| format!("Cannot locate private app data: {error}"))?;
    let resolved = fs::canonicalize(&marker)
        .map_err(|error| format!("Cannot resolve private app data: {error}"));
    drop(file);
    let cleanup = fs::remove_file(&marker)
        .map_err(|error| format!("Cannot remove private location marker: {error}"));
    let resolved = resolved?;
    cleanup?;
    resolved
        .parent()
        .map(Path::to_path_buf)
        .ok_or_else(|| "Cannot locate the private app data directory.".into())
}

fn write_atomic(folder: &Path, filename: &str, bytes: &[u8]) -> Result<PathBuf, String> {
    if bytes.len() > MAX_BYTES {
        return Err("The saved file exceeds the 1 MiB limit.".into());
    }
    let base = fs::canonicalize(folder).map_err(|error| error.to_string())?;
    let target = base.join(filename);
    if target.exists() {
        let resolved = fs::canonicalize(&target).map_err(|error| error.to_string())?;
        if resolved.parent() != Some(base.as_path()) {
            return Err("The existing output resolves outside its private folder.".into());
        }
    }
    let counter = NEXT_WRITE.fetch_add(1, Ordering::Relaxed);
    let temporary = base.join(format!(
        "entities-write-{}-{counter}.tmp",
        std::process::id()
    ));
    let mut file = OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(&temporary)
        .map_err(|error| format!("Cannot create temporary output: {error}"))?;
    let result = (|| {
        file.write_all(bytes).map_err(|error| error.to_string())?;
        file.sync_all().map_err(|error| error.to_string())?;
        drop(file);
        fs::rename(&temporary, &target)
            .map_err(|error| format!("Cannot finish saving: {error}"))?;
        Ok(target)
    })();
    if result.is_err() {
        // This is exactly the owned create_new temporary file above.
        let _ = fs::remove_file(&temporary);
    }
    result
}

pub fn save_session(root: &Path, document: Value) -> Result<(), String> {
    if !document.is_object() {
        return Err("The session must be an object.".into());
    }
    let bytes = serde_json::to_vec_pretty(&document).map_err(|error| error.to_string())?;
    write_atomic(&private_folder(root, &[])?, "session.json", &bytes)?;
    Ok(())
}

pub fn load_session(root: &Path) -> Result<Option<Value>, String> {
    let base = private_folder(root, &[])?;
    let path = base.join("session.json");
    if !path.exists() {
        return Ok(None);
    }
    let resolved = fs::canonicalize(path).map_err(|error| error.to_string())?;
    if resolved.parent() != Some(base.as_path()) {
        return Err("The session resolves outside its private folder.".into());
    }
    let value: Value = serde_json::from_slice(&read_limited(&resolved)?)
        .map_err(|error| format!("Cannot read session: {error}"))?;
    if !value.is_object() {
        return Err("The session has an unsupported format.".into());
    }
    Ok(Some(value))
}
