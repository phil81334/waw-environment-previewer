use crate::assets::{self, AssetReader};
use crate::{scene, BakedScene, ProbeSettings, DEFINITIONS};
use std::collections::BTreeSet;
use std::fs::{self, File};
#[cfg(windows)]
use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

fn cancelled(cancel: &AtomicBool) -> Result<(), String> {
    if cancel.load(Ordering::Relaxed) {
        Err("Bake cancelled. The previous scene is retained.".into())
    } else {
        Ok(())
    }
}

// Other previewer processes share the cache. Only jobs that have left scope
// are eligible for cleanup, including cancelled builds with diagnostic logs.
struct ActiveJob<'a>(&'a Path);
impl Drop for ActiveJob<'_> {
    fn drop(&mut self) {
        let _ = File::create(self.0.join("finished"));
    }
}

pub(crate) fn bake_entities_source(
    game: &Path,
    jobs: &Path,
    settings: &ProbeSettings,
    map: &str,
    cancel: &AtomicBool,
    progress: &dyn Fn(String),
) -> Result<(BakedScene, Vec<u8>), String> {
    cancelled(cancel)?;
    assets::verify_tools(game)?;
    let game = game.canonicalize().map_err(|e| e.to_string())?;
    fs::create_dir_all(jobs).map_err(|e| format!("Cannot create the private bake cache: {e}"))?;
    let jobs = jobs.canonicalize().map_err(|e| e.to_string())?;
    if jobs.starts_with(&game) {
        return Err("The bake cache must be outside the game installation.".into());
    }
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|e| e.to_string())?
        .as_millis();
    let job = jobs.join(format!("bake-{timestamp}-{}", std::process::id()));
    fs::create_dir(&job).map_err(|e| format!("Cannot create a private bake job: {e}"))?;
    let active = ActiveJob(&job);
    let raw = job.join("raw");
    fs::create_dir_all(raw.join("maps")).map_err(|e| e.to_string())?;
    fs::create_dir_all(job.join("map_source")).map_err(|e| e.to_string())?;
    let tools = job.join("tools");
    fs::create_dir_all(&tools).map_err(|e| e.to_string())?;
    // Both compilers load the runtime DLL from their own directory.
    for name in ["cod2map.exe", "cod2rad.exe", "MSVCR71.dll"] {
        fs::copy(game.join("bin").join(name), tools.join(name))
            .map_err(|e| format!("Cannot stage Modtools dependency {name}: {e}"))?;
    }
    assets::verify_tool_directory(&tools)?;
    fs::write(
        job.join("settings.json"),
        serde_json::to_vec_pretty(settings).map_err(|e| e.to_string())?,
    )
    .map_err(|e| e.to_string())?;
    progress("Preparing a private scene and original assets…".into());
    let source_hash = assets::sha(map.as_bytes());
    fs::write(job.join("map_source/lights_lab.map"), map.as_bytes()).map_err(|e| e.to_string())?;
    let mut reader = AssetReader::new_entities(&game)?;
    cancelled(cancel)?;
    let mut materials = world_materials(map)?;
    materials.insert("$default".into());
    materials.insert("lightmap_gray".into());
    for name in materials {
        cancelled(cancel)?;
        progress(format!("Preparing material {name}…"));
        let material = reader.material(&name)?;
        reader.copy_to(&format!("materials/{name}"), &raw)?;
        for binding in material.textures {
            reader.copy_to(&format!("images/{}.iwi", binding.texture_id), &raw)?;
        }
    }
    for definition in DEFINITIONS {
        cancelled(cancel)?;
        let bytes = reader.copy_to(&format!("lights/{definition}"), &raw)?;
        if bytes.first() != Some(&0x62) {
            return Err(format!(
                "Light definition {definition} uses an unsupported sampler."
            ));
        }
        let image = assets::string_at(&bytes, 1)?;
        assets::name(&image)?;
        reader.copy_to(&format!("images/{image}.iwi"), &raw)?;
    }
    let prefab_hashes = crate::entities::prepare_dependencies(&game, &job, map, &mut reader)?;
    reader.copy_to("fileSysCheck.cfg", &raw)?;
    cancelled(cancel)?;
    run_tool(
        &job,
        "map",
        &[
            "-platform",
            "pc",
            "-loadFrom",
            "map_source/lights_lab.map",
            "raw/maps/lights_lab",
        ],
        cancel,
        progress,
    )?;
    cancelled(cancel)?;
    run_tool(
        &job,
        "rad",
        &[
            "-platform",
            "pc",
            "-fast",
            "-threads",
            "1",
            "-noRelight",
            "-traces",
            "32",
            "-jitter",
            "0",
            "-superSample",
            "4",
            "-maxBounces",
            "8",
            "raw/maps/lights_lab",
        ],
        cancel,
        progress,
    )?;
    cancelled(cancel)?;
    progress("Validating the complete baked scene…".into());
    let bsp = fs::read(raw.join("maps/lights_lab.d3dbsp"))
        .map_err(|e| format!("The compiler output is missing: {e}"))?;
    let mut baked = scene::read_entities_bsp(&bsp, &settings.scene, source_hash, &mut reader)?;
    baked.prefab_hashes = prefab_hashes;
    crate::models::append_models(&bsp, &mut reader, &mut baked)?;
    crate::lightgrid::decorate_models(&bsp, &mut baked)?;
    for stage in ["map", "rad"] {
        let log = fs::read_to_string(job.join(format!("{stage}.log"))).unwrap_or_default();
        for line in log.lines().filter(|line| {
            let lower = line.to_ascii_lowercase();
            lower.contains("warning") || lower.contains("sunradiosity") && lower.contains("rais")
        }) {
            if baked.manifest.warnings.len() < 40 {
                baked.manifest.warnings.push(line.trim().to_owned());
            }
        }
    }
    cancelled(cancel)?;
    fs::write(
        job.join("manifest.json"),
        serde_json::to_vec_pretty(&baked.manifest).map_err(|e| e.to_string())?,
    )
    .map_err(|e| e.to_string())?;
    drop(active);
    retain_recent_jobs(&jobs, &job);
    progress(format!(
        "Bake complete: {} triangles.",
        baked.manifest.geometry.index_count / 3
    ));
    Ok((baked, bsp))
}

fn run_tool(
    job: &Path,
    stage: &str,
    arguments: &[&str],
    cancel: &AtomicBool,
    progress: &dyn Fn(String),
) -> Result<(), String> {
    cancelled(cancel)?;
    let tool = if stage == "map" {
        "cod2map.exe"
    } else {
        "cod2rad.exe"
    };
    let log_path = job.join(format!("{stage}.log"));
    let log = File::create(&log_path).map_err(|e| e.to_string())?;
    let stderr = log.try_clone().map_err(|e| e.to_string())?;
    let mut command = Command::new(job.join("tools").join(tool));
    command
        .args(arguments)
        .current_dir(job)
        .stdin(Stdio::null())
        .stdout(Stdio::from(log))
        .stderr(Stdio::from(stderr));
    #[cfg(windows)]
    command.creation_flags(0x08000000);
    progress(if stage == "map" {
        "Compiling scene geometry…".into()
    } else {
        "Baking lighting with the original compiler…".into()
    });
    let mut child = command
        .spawn()
        .map_err(|e| format!("Cannot start original {tool}: {e}"))?;
    let start = Instant::now();
    let mut last_update = Instant::now();
    let status = loop {
        if cancel.load(Ordering::Relaxed) || start.elapsed() > Duration::from_secs(120) {
            let _ = child.kill();
            let _ = child.wait();
            return Err(if cancel.load(Ordering::Relaxed) {
                "Bake cancelled. The previous scene is retained.".into()
            } else {
                format!("{tool} exceeded the two-minute limit; the previous scene is retained.")
            });
        }
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) => {}
            Err(error) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(format!("Cannot monitor original {tool}: {error}"));
            }
        }
        if last_update.elapsed() > Duration::from_secs(1) {
            progress(format!(
                "{}… {}s",
                if stage == "map" {
                    "Compiling geometry"
                } else {
                    "Baking lighting"
                },
                start.elapsed().as_secs()
            ));
            last_update = Instant::now();
        }
        thread::sleep(Duration::from_millis(20));
    };
    let bytes = fs::read(&log_path).map_err(|e| e.to_string())?;
    let log = String::from_utf8_lossy(&bytes);
    let completion = if stage == "map" {
        "Writing "
    } else {
        "Entire light compile finished"
    };
    if !status.success() || !log.contains(completion) || compiler_failure(&log) {
        let detail = log
            .lines()
            .rev()
            .filter(|line| !line.trim().is_empty())
            .take(12)
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect::<Vec<_>>()
            .join("\n");
        return Err(format!(
            "Original {tool} failed; previous scene retained.\n{detail}\nLog: {}",
            log_path.display()
        ));
    }
    Ok(())
}

fn compiler_failure(log: &str) -> bool {
    let lower = log.to_ascii_lowercase();
    lower.contains("******* leaked")
        || lower.contains("fatal error")
        || lower
            .lines()
            .any(|line| line.trim_start().starts_with("error:"))
}

fn retain_recent_jobs(jobs: &Path, current: &Path) {
    let Ok(entries) = fs::read_dir(jobs) else {
        return;
    };
    let mut candidates: Vec<PathBuf> = entries
        .filter_map(Result::ok)
        .filter_map(|entry| {
            let path = entry.path();
            let kind = entry.file_type().ok()?;
            let name = entry.file_name();
            let name = name.to_string_lossy();
            let owned_name = name
                .strip_prefix("bake-")
                .and_then(|value| value.split_once('-'))
                .is_some_and(|(time, pid)| {
                    !time.is_empty()
                        && !pid.is_empty()
                        && time
                            .bytes()
                            .chain(pid.bytes())
                            .all(|byte| byte.is_ascii_digit())
                });
            if owned_name
                && kind.is_dir()
                && !kind.is_symlink()
                && path.canonicalize().ok().as_deref() == Some(path.as_path())
                && path.join("finished").is_file()
            {
                Some(path)
            } else {
                None
            }
        })
        .collect();
    candidates.sort();
    let remove = candidates.len().saturating_sub(6);
    for path in candidates.into_iter().take(remove) {
        if path != current && path.parent() == Some(jobs) {
            let _ = fs::remove_dir_all(path);
        }
    }
}

pub(crate) fn top_entities(source: &str) -> Result<Vec<&str>, String> {
    let bytes = source.as_bytes();
    let (mut depth, mut start, mut quote, mut comment, mut escape) =
        (0usize, 0usize, false, false, false);
    let mut result = Vec::new();
    for (index, &byte) in bytes.iter().enumerate() {
        if comment {
            if byte == b'\n' {
                comment = false;
            }
            continue;
        }
        if quote {
            if escape {
                escape = false;
            } else if byte == b'\\' {
                escape = true;
            } else if byte == b'"' {
                quote = false;
            }
            continue;
        }
        if byte == b'/' && bytes.get(index + 1) == Some(&b'/') {
            comment = true;
            continue;
        }
        if byte == b'"' {
            quote = true;
            continue;
        }
        if byte == b'{' {
            if depth == 0 {
                start = index;
            }
            depth += 1;
        }
        if byte == b'}' {
            if depth == 0 {
                return Err("Unbalanced MAP entity braces.".into());
            }
            depth -= 1;
            if depth == 0 {
                result.push(&source[start..index + 1]);
            }
        }
    }
    if depth != 0 || quote {
        return Err("Unterminated MAP entity or string.".into());
    }
    Ok(result)
}

pub(crate) fn world_materials(world: &str) -> Result<BTreeSet<String>, String> {
    let mut names = BTreeSet::new();
    for line in world.lines() {
        if !line.trim_start().starts_with('(') {
            continue;
        }
        let Some(end) = line.rfind(')') else {
            continue;
        };
        if let Some(name) = line[end + 1..].split_whitespace().next() {
            assets::name(name)?;
            names.insert(name.to_owned());
        }
    }
    if names.is_empty() {
        return Err("The scene has no supported world brushes.".into());
    }
    Ok(names)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn compiler_success_markers_do_not_override_leak_errors() {
        assert!(compiler_failure(
            "******* leaked *******\nWriting file\nEntire light compile finished"
        ));
        assert!(!compiler_failure("0 errors\nWriting file"));
    }
    #[test]
    fn entity_parser_preserves_nested_world_and_ignores_comment_braces() {
        let source = "iwmap 4\n// { ignored\n{\n\"classname\" \"worldspawn\"\n{\n}\n}\n{\n\"classname\" \"light\"\n}\n";
        let parts = top_entities(source).unwrap();
        assert_eq!(parts.len(), 2);
        assert!(parts[0].contains("{\n}"));
        assert!(top_entities("{\n").is_err());
    }
}
