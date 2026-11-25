//! Fixed-scene editing inputs. These modify only the private compilation copy.
use crate::{bake, worldspawn, WorldspawnSettings};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AuthoredLightEdit {
    /// Zero-based light order in the scene source, independent of compiled primary indices.
    pub index: usize,
    pub color: [f32; 3],
    pub intensity: f32,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct EnvironmentSettings {
    #[serde(default)]
    pub worldspawn: Option<WorldspawnSettings>,
    #[serde(default)]
    pub lights: Vec<AuthoredLightEdit>,
}

pub(crate) fn validate(settings: &EnvironmentSettings, scene: &str) -> Result<(), String> {
    if let Some(world) = &settings.worldspawn {
        worldspawn::validate_worldspawn(world)?;
        if world.scene != scene {
            return Err("Environment worldspawn must belong to the selected scene.".into());
        }
    }
    if settings.lights.len() > 128 {
        return Err("Environment edits support at most 128 authored lights.".into());
    }
    let mut indexes = HashSet::new();
    for light in &settings.lights {
        if light.index >= 128 || !indexes.insert(light.index) {
            return Err("Environment light indexes must be unique and below 128.".into());
        }
        if !light.intensity.is_finite()
            || !(0.0..=4.0).contains(&light.intensity)
            || light
                .color
                .iter()
                .any(|v| !v.is_finite() || !(0.0..=1.0).contains(v))
            || light.color.iter().all(|v| *v == 0.0)
        {
            return Err("Light colour channels must be 0–1 with a nonzero channel; intensity must be 0–4. Use intensity zero to disable a light.".into());
        }
    }
    Ok(())
}

fn property<'a>(properties: &'a [[String; 2]], key: &str) -> Option<&'a str> {
    properties
        .iter()
        .find(|p| p[0] == key)
        .map(|p| p[1].as_str())
}

fn vector_property(properties: &[[String; 2]], key: &str) -> Option<[f32; 3]> {
    let values = property(properties, key)?
        .split_whitespace()
        .map(str::parse::<f32>)
        .collect::<Result<Vec<_>, _>>()
        .ok()?;
    if values.len() != 3 || values.iter().any(|value| !value.is_finite()) {
        return None;
    }
    Some([values[0], values[1], values[2]])
}

pub(crate) fn inspect_lights(source: &str) -> Result<Vec<serde_json::Value>, String> {
    let entities = bake::top_entities(source)?
        .into_iter()
        .map(worldspawn::properties)
        .collect::<Result<Vec<_>, _>>()?;
    let mut lights = Vec::new();
    for properties in &entities {
        if property(properties, "classname") != Some("light") {
            continue;
        }
        let index = lights.len();
        let origin = vector_property(properties, "origin").unwrap_or([0.0; 3]);
        let color = vector_property(properties, "_color").unwrap_or([1.0; 3]);
        let intensity = property(properties, "intensity")
            .and_then(|v| v.parse::<f32>().ok())
            .filter(|v| v.is_finite())
            .unwrap_or(1.0);
        let radius = property(properties, "radius")
            .and_then(|v| v.parse::<f32>().ok())
            .filter(|v| v.is_finite() && *v > 0.0)
            .unwrap_or(200.0);
        // The authored info_null is the spotlight's positional aim target.
        // Do not guess gameplay-driven targets or reuse another light's target.
        let target = property(properties, "target").and_then(|name| {
            entities
                .iter()
                .find(|candidate| {
                    property(candidate, "classname") == Some("info_null")
                        && property(candidate, "targetname") == Some(name)
                })
                .and_then(|candidate| vector_property(candidate, "origin"))
        });
        let direction = target.and_then(|target| {
            let delta = [
                target[0] - origin[0],
                target[1] - origin[1],
                target[2] - origin[2],
            ];
            let length = delta.iter().map(|v| v * v).sum::<f32>().sqrt();
            (length.is_finite() && length >= 1.0)
                .then(|| [delta[0] / length, delta[1] / length, delta[2] / length])
        });
        lights.push(
            serde_json::json!({"index":index,"origin":origin,"target":target,
            "direction":direction,"radius":radius,"color":color,"intensity":intensity,
            "label":format!("Light {} · {} {} {}", index + 1, origin[0], origin[1], origin[2])}),
        );
    }
    Ok(lights)
}

pub(crate) fn apply_light_edits(
    source: &str,
    edits: &[AuthoredLightEdit],
) -> Result<String, String> {
    validate(
        &EnvironmentSettings {
            worldspawn: None,
            lights: edits.to_vec(),
        },
        &crate::scene_profile::active()?.id,
    )?;
    if edits.is_empty() {
        return Ok(source.to_owned());
    }
    let entities = bake::top_entities(source)?;
    let mut output = String::with_capacity(source.len());
    let mut cursor = 0;
    let mut light_index = 0;
    let mut applied = 0;
    for entity in entities {
        let start = entity.as_ptr() as usize - source.as_ptr() as usize;
        output.push_str(&source[cursor..start]);
        let properties = worldspawn::properties(entity)?;
        if property(&properties, "classname") == Some("light") {
            if let Some(edit) = edits.iter().find(|e| e.index == light_index) {
                // Lights are point entities. Preserve every other authored property,
                // including target, spawnflags, radius and cone, at the same order.
                let mut properties = properties;
                properties.retain(|p| p[0] != "_color" && p[0] != "intensity");
                properties.push([
                    "_color".into(),
                    format!("{} {} {}", edit.color[0], edit.color[1], edit.color[2]),
                ]);
                properties.push(["intensity".into(), edit.intensity.to_string()]);
                output.push_str("{\n");
                for [key, value] in properties {
                    if key
                        .bytes()
                        .chain(value.bytes())
                        .any(|b| b == b'"' || b == b'\\' || b.is_ascii_control())
                    {
                        return Err(
                            "Unsupported quoted/control text in authored light property.".into(),
                        );
                    }
                    output.push_str(&format!("\"{key}\" \"{value}\"\n"));
                }
                output.push_str("}");
                applied += 1;
            } else {
                output.push_str(entity);
            }
            light_index += 1;
        } else {
            output.push_str(entity);
        }
        cursor = start + entity.len();
    }
    output.push_str(&source[cursor..]);
    if applied != edits.len() {
        return Err(
            "An edited light is no longer present in the fixed scene. Reload the scene settings."
                .into(),
        );
    }
    Ok(output)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn authored_light_edit_preserves_target_and_other_entities() {
        let source = "iwmap 4\n{\n\"classname\" \"worldspawn\"\n}\n{\n\"classname\" \"light\"\n\"target\" \"aim\"\n\"origin\" \"1 2 3\"\n\"_color\" \"1 1 1\"\n}\n{\n\"classname\" \"info_null\"\n\"targetname\" \"aim\"\n}\n";
        let result = apply_light_edits(
            source,
            &[AuthoredLightEdit {
                index: 0,
                color: [1.0, 0.5, 0.25],
                intensity: 0.0,
            }],
        )
        .unwrap();
        assert!(result.contains("\"target\" \"aim\""));
        assert!(result.contains("\"targetname\" \"aim\""));
        assert!(result.contains("\"intensity\" \"0\""));
        assert!(source.contains("\"_color\" \"1 1 1\""));
    }
    #[test]
    fn light_focus_targets_are_authored_and_independent() {
        let source = "iwmap 4\n{\n\"classname\" \"worldspawn\"\n}\n{\n\"classname\" \"light\"\n\"origin\" \"1 2 10\"\n\"target\" \"aim\"\n\"radius\" \"300\"\n}\n{\n\"classname\" \"light\"\n\"origin\" \"4 5 6\"\n\"target\" \"missing\"\n}\n{\n\"classname\" \"info_null\"\n\"targetname\" \"aim\"\n\"origin\" \"1 2 0\"\n}\n";
        let lights = inspect_lights(source).unwrap();
        assert_eq!(lights[0]["target"], serde_json::json!([1.0, 2.0, 0.0]));
        assert_eq!(lights[0]["direction"], serde_json::json!([0.0, 0.0, -1.0]));
        assert_eq!(lights[0]["radius"], serde_json::json!(300.0));
        assert!(lights[1]["target"].is_null());
        assert!(lights[1]["direction"].is_null());
    }
    #[test]
    fn stale_duplicate_or_nonfinite_edits_are_rejected() {
        let edit = AuthoredLightEdit {
            index: 0,
            color: [1.0; 3],
            intensity: 1.0,
        };
        assert!(apply_light_edits(
            "iwmap 4\n{\n\"classname\" \"worldspawn\"\n}\n",
            &[edit.clone()]
        )
        .is_err());
        assert!(validate(
            &EnvironmentSettings {
                worldspawn: None,
                lights: vec![edit.clone(), edit]
            },
            crate::scene_profile::active().unwrap().id.as_str()
        )
        .is_err());
        let bad = AuthoredLightEdit {
            index: 0,
            color: [f32::NAN, 1.0, 1.0],
            intensity: 1.0,
        };
        assert!(validate(
            &EnvironmentSettings {
                worldspawn: None,
                lights: vec![bad]
            },
            crate::scene_profile::active().unwrap().id.as_str()
        )
        .is_err());
    }
}
