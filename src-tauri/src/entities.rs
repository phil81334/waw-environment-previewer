//! Inspect and compile the visible entities from the selected scene.
use crate::{
    assets::{self, AssetReader},
    bake, probes, scene_profile, worldspawn, BakedScene, ProbeSettings, WorldspawnSettings,
};
use std::{
    collections::BTreeMap,
    fs,
    path::{Component, Path},
    sync::atomic::AtomicBool,
};

pub fn inspect_entities(game: &Path) -> Result<serde_json::Value, String> {
    assets::verify_tools(game)?;
    let world = worldspawn::defaults()?;
    let mut reader = AssetReader::new_entities(game)?;
    let skies = reader.installed_sky_models()?;
    let (corrections, warnings) = probes::corrections(&mut reader)?;
    let probes = probes::defaults()?;
    Ok(serde_json::json!({
        "skyModels": skies,
        "corrections": corrections,
        "warnings": warnings,
        "probeDefaults": { (world.scene.clone()): probes },
        "environmentDefaults": {
            "worldspawn": world,
            "lights": crate::environment::inspect_lights(scene_profile::SCENE_MAP)?
        }
    }))
}

pub fn bake_entities(
    game: &Path,
    jobs: &Path,
    settings: &ProbeSettings,
    cancel: &AtomicBool,
    progress: &dyn Fn(String),
) -> Result<BakedScene, String> {
    probes::bake_with_entities(game, jobs, settings, cancel, progress)
}

pub(crate) fn source_map(settings: &WorldspawnSettings) -> Result<String, String> {
    let mut output = worldspawn::source_map(settings)?;
    for entity in bake::top_entities(scene_profile::SCENE_MAP)? {
        let properties = worldspawn::properties(entity)?;
        let class = properties
            .iter()
            .find(|p| p[0] == "classname")
            .map(|p| p[1].as_str())
            .unwrap_or("");
        // Already emitted with the edited worldspawn, or replaced by the profile probe.
        if matches!(
            class,
            "worldspawn"
                | "func_group"
                | "func_cullgroup"
                | "light"
                | "info_null"
                | "info_player_start"
                | "reflection_probe"
        ) {
            continue;
        }
        if matches!(class, "misc_model" | "script_model" | "misc_prefab")
            || entity[1..].contains('{')
        {
            output.push_str(entity);
            output.push('\n');
        }
    }
    Ok(output)
}

pub(crate) fn prepare_dependencies(
    game: &Path,
    job: &Path,
    map: &str,
    assets: &mut AssetReader,
) -> Result<BTreeMap<String, String>, String> {
    let raw = job.join("raw");
    crate::models::prepare_compiler_assets(map, assets, &raw)?;
    let mut pending = vec![map.to_owned()];
    let mut copied = BTreeMap::new();
    while let Some(source) = pending.pop() {
        for entity in bake::top_entities(&source)? {
            let properties = worldspawn::properties(entity)?;
            if !properties
                .iter()
                .any(|p| p[0] == "classname" && p[1] == "misc_prefab")
            {
                continue;
            }
            let name = properties
                .iter()
                .find(|p| p[0] == "model")
                .map(|p| p[1].replace('\\', "/"))
                .ok_or("Prefab has no source reference.")?;
            let relative = name.strip_prefix("map_source/").unwrap_or(&name);
            if relative.is_empty()
                || !relative.ends_with(".map")
                || Path::new(relative)
                    .components()
                    .any(|part| !matches!(part, Component::Normal(_)))
            {
                return Err("Unsafe prefab source reference.".into());
            }
            if copied.contains_key(relative) {
                continue;
            }
            if copied.len() >= 64 {
                return Err("The fixed scene exceeds the prefab dependency limit.".into());
            }
            let source_root = game
                .join("map_source")
                .canonicalize()
                .map_err(|e| format!("Cannot locate map_source for prefab {relative}: {e}"))?;
            if !source_root.starts_with(game) {
                return Err("Prefab directory is outside the installed game.".into());
            }
            let original = source_root
                .join(relative)
                .canonicalize()
                .map_err(|e| format!("Cannot read prefab {relative}: {e}"))?;
            if !original.starts_with(&source_root) {
                return Err("Prefab resolves outside map_source.".into());
            }
            use std::io::Read;
            let file = fs::File::open(&original).map_err(|e| e.to_string())?;
            let mut bytes = Vec::new();
            file.take(16 * 1024 * 1024 + 1)
                .read_to_end(&mut bytes)
                .map_err(|e| e.to_string())?;
            if bytes.len() > 16 * 1024 * 1024 {
                return Err("Prefab exceeds the supported size limit.".into());
            }
            copied.insert(relative.to_owned(), assets::sha(&bytes));
            let text =
                String::from_utf8(bytes).map_err(|_| "Prefab source must be UTF-8/ASCII.")?;
            let destination = job.join("map_source").join(relative);
            fs::create_dir_all(destination.parent().ok_or("Invalid prefab destination.")?)
                .map_err(|e| e.to_string())?;
            fs::write(destination, &text).map_err(|e| e.to_string())?;
            for material in bake::world_materials(&text)? {
                assets::name(&material)?;
                assets.copy_to(&format!("materials/{material}"), &raw)?;
                for binding in assets.material(&material)?.textures {
                    assets.copy_to(&format!("images/{}.iwi", binding.texture_id), &raw)?;
                }
            }
            crate::models::prepare_compiler_assets(&text, assets, &raw)?;
            pending.push(text);
        }
    }
    Ok(copied)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn edited_source_keeps_authored_lights_and_uses_the_profile_probe() {
        let source = source_map(&worldspawn::defaults().unwrap()).unwrap();
        assert_eq!(
            crate::environment::inspect_lights(&source).unwrap(),
            crate::environment::inspect_lights(scene_profile::SCENE_MAP).unwrap()
        );
        // The compiler path appends the active profile's probe after this step.
        for entity in bake::top_entities(&source).unwrap() {
            assert!(!worldspawn::properties(entity)
                .unwrap()
                .iter()
                .any(|[key, value]| key == "classname" && value == "reflection_probe"));
        }
    }
}
