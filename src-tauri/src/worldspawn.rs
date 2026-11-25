//! Editable worldspawn properties for the embedded scene.
use crate::{
    assets::{self, AssetReader},
    bake, scene_profile,
};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WorldspawnSettings {
    pub id: String,
    pub scene: String,
    pub properties: Vec<[String; 2]>,
}

impl WorldspawnSettings {
    pub fn get(&self, key: &str) -> Option<&str> {
        self.properties
            .iter()
            .find(|pair| pair[0] == key)
            .map(|pair| pair[1].as_str())
    }
}

pub fn validate_worldspawn(settings: &WorldspawnSettings) -> Result<(), String> {
    if settings.id.is_empty()
        || settings.id.len() > 64
        || !settings
            .id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-')
    {
        return Err("Invalid worldspawn document identifier.".into());
    }
    if settings.scene != scene_profile::active()?.id || settings.properties.len() > 128 {
        return Err("Choose a supported scene with no more than 128 worldspawn properties.".into());
    }
    let mut keys = HashSet::new();
    for [key, value] in &settings.properties {
        if key.is_empty() || key.len() > 96 || value.len() > 256 || !keys.insert(key) {
            return Err("Worldspawn keys must be unique and fit the supported size limit.".into());
        }
        if key
            .bytes()
            .chain(value.bytes())
            .any(|b| !b.is_ascii() || b.is_ascii_control() || b == b'"' || b == b'\\')
        {
            return Err("Property text must be plain ASCII without quotes, backslashes or control characters.".into());
        }
        if key == "classname" && value != "worldspawn" {
            return Err("The classname must remain worldspawn.".into());
        }
        if key == "skyboxmodel" && !value.is_empty() {
            assets::name(value)?;
        }
        let count = match key.as_str() {
            "suncolor" | "sundiffusecolor" | "_color" | "sundirection" => 3,
            "sunlight"
            | "sunradiosity"
            | "ambient"
            | "diffusefraction"
            | "radiosityscale"
            | "contrastgain"
            | "northyaw"
            | "treeScatterIntensity"
            | "treeScatterAmount"
            | "sunIsPrimaryLight" => 1,
            _ => continue,
        };
        // An empty authored value and an omitted property remain distinct.
        // The original compiler, rather than this reader, supplies defaults.
        if value.is_empty() {
            continue;
        }
        let values: Vec<f32> = value
            .split_whitespace()
            .map(str::parse::<f32>)
            .collect::<Result<_, _>>()
            .map_err(|_| format!("{key} needs {count} numeric value(s)."))?;
        if values.len() != count || values.iter().any(|v| !v.is_finite() || v.abs() > 36000.0) {
            return Err(format!(
                "{key} needs {count} finite value(s) within the playground range."
            ));
        }
        if ["suncolor", "sundiffusecolor", "_color"].contains(&key.as_str())
            && values.iter().any(|v| !(0.0..=1.0).contains(v))
        {
            return Err(format!("{key} colour channels must be 0–1."));
        }
        if key == "sunIsPrimaryLight" && ![0.0, 1.0].contains(&values[0]) {
            return Err("sunIsPrimaryLight must be 0 or 1.".into());
        }
        if [
            "sunlight",
            "ambient",
            "diffusefraction",
            "radiosityscale",
            "contrastgain",
            "treeScatterIntensity",
            "treeScatterAmount",
        ]
        .contains(&key.as_str())
            && !(0.0..=16.0).contains(&values[0])
        {
            return Err(format!("{key} must be 0–16 in this playground."));
        }
        if key == "sunradiosity" && !(-16.0..=16.0).contains(&values[0]) {
            return Err("sunradiosity must be −16 to 16 in this playground.".into());
        }
    }
    Ok(())
}

pub(crate) fn defaults() -> Result<WorldspawnSettings, String> {
    let profile = scene_profile::active()?;
    let entities = bake::top_entities(scene_profile::SCENE_MAP)?;
    let first = entities.first().ok_or("The scene MAP has no entities.")?;
    let mut properties = properties(first)?;
    if !properties
        .iter()
        .any(|[key, value]| key == "classname" && value == "worldspawn")
    {
        return Err("The first MAP entity must be worldspawn.".into());
    }
    if !properties.iter().any(|[key, _]| key == "skyboxmodel") && !profile.default_sky.is_empty() {
        properties.push(["skyboxmodel".into(), profile.default_sky.clone()]);
    }
    Ok(WorldspawnSettings {
        id: format!("{}-worldspawn", profile.id),
        scene: profile.id.clone(),
        properties,
    })
}

pub(crate) fn sky_models(reader: &mut AssetReader, world: &WorldspawnSettings) -> Vec<String> {
    let mut names = Vec::new();
    // These families have a verified shader path. Also offer the scene author's
    // own sky if installed; the renderer validates its materials when baking.
    for name in std::iter::once(world.get("skyboxmodel").unwrap_or("")).chain([
        "skybox_pel1",
        "skybox_zombie",
        "skybox_fly",
    ]) {
        if !name.is_empty()
            && !names.iter().any(|n| n == name)
            && reader.read(&format!("xmodel/{name}")).is_ok()
        {
            names.push(name.to_owned());
        }
    }
    names
}

fn header(settings: &WorldspawnSettings) -> String {
    let mut text = String::from("{\n");
    if settings.get("classname").is_none() {
        text.push_str("\"classname\" \"worldspawn\"\n");
    }
    for [key, value] in &settings.properties {
        text.push_str(&format!("\"{key}\" \"{value}\"\n"));
    }
    text
}

pub(crate) fn properties(entity: &str) -> Result<Vec<[String; 2]>, String> {
    let mut result = Vec::new();
    for line in entity.lines().skip(1) {
        let line = line.trim();
        if line.starts_with('{') {
            break;
        }
        if !line.starts_with('"') {
            continue;
        }
        let pieces: Vec<&str> = line.split('"').collect();
        if pieces.len() != 5 {
            return Err("Unsupported MAP property syntax.".into());
        }
        result.push([pieces[1].into(), pieces[3].into()]);
    }
    Ok(result)
}

pub(crate) fn source_map(settings: &WorldspawnSettings) -> Result<String, String> {
    let original = scene_profile::SCENE_MAP;
    let entities = bake::top_entities(original)?;
    let first = entities.first().ok_or("The scene MAP has no entities.")?;
    // Keep the original Radiant layer declarations before the first entity.
    let header_size = first.as_ptr() as usize - original.as_ptr() as usize;
    let mut source = String::from(&original[..header_size]);
    let mut world_found = false;
    for entity in entities {
        let props = properties(entity)?;
        let class = props
            .iter()
            .find(|p| p[0] == "classname")
            .map(|p| p[1].as_str())
            .unwrap_or("");
        if class == "worldspawn" {
            if world_found {
                return Err("The scene has multiple worldspawn entities.".into());
            }
            // Locate the first nested opening independent of line endings.
            let nested = entity[1..]
                .find('{')
                .map(|n| n + 1)
                .ok_or("Missing scene brushes.")?;
            source.push_str(&header(settings));
            source.push_str(&entity[nested..]);
            source.push('\n');
            world_found = true;
        } else if [
            "func_group",
            "func_cullgroup",
            "light",
            "info_null",
            "info_player_start",
        ]
        .contains(&class)
        {
            source.push_str(entity);
            source.push('\n');
        }
    }
    if !world_found {
        return Err("The configured scene lacks worldspawn architecture.".into());
    }
    Ok(source)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn parsed_worldspawn_preserves_empty_values_and_rejects_injected_text() {
        let mut settings = defaults().unwrap();
        settings
            .properties
            .push(["preview_test_property ".into(), "".into()]);
        assert!(validate_worldspawn(&settings).is_ok());
        let text = header(&settings);
        assert_eq!(properties(&(text + "{}\n}")).unwrap(), settings.properties);
        settings.properties.last_mut().unwrap()[1] = "bad\"\nvalue".into();
        assert!(validate_worldspawn(&settings).is_err());
    }
}
