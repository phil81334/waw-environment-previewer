//! Fixed-scene, first-generation reflection captures. The original compiler
//! chooses surface assignments; the browser captures only fresh black seeds.
use crate::assets::{f32_at, string_at, u32_at, AssetReader};
use crate::{bake, scene_profile, sky, worldspawn, BakedScene};
use serde::{Deserialize, Serialize};
use std::{
    path::Path,
    sync::atomic::{AtomicBool, Ordering},
};

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Probe {
    pub origin: [f32; 3],
    pub correction: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ProbeSettings {
    pub id: String,
    pub scene: String,
    pub global_correction: String,
    pub probes: Vec<Probe>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub environment: Option<crate::EnvironmentSettings>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CompiledProbe {
    pub index: usize,
    pub origin: [f32; 3],
    pub correction: String,
}

fn valid_name(name: &str) -> bool {
    name.len() <= 63
        && name
            .bytes()
            .all(|b| b.is_ascii() && !b.is_ascii_control() && b != b'"' && b != b'\\')
}

pub fn validate_probes(settings: &ProbeSettings) -> Result<(), String> {
    if settings.id.is_empty()
        || settings.id.len() > 64
        || !settings
            .id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-')
    {
        return Err("Invalid probe document identifier.".into());
    }
    if settings.scene != scene_profile::active()?.id || settings.probes.len() != 1 {
        return Err("The configured scene uses one reflection probe.".into());
    }
    if !valid_name(&settings.global_correction)
        || settings.probes.iter().any(|p| !valid_name(&p.correction))
    {
        return Err("Correction names must be at most 63 ASCII bytes, without quotes, backslashes or control characters.".into());
    }
    if settings
        .probes
        .iter()
        .any(|p| p.origin.iter().any(|v| !v.is_finite() || v.abs() > 16384.0))
    {
        return Err("Probe positions must be finite and between −16384 and 16384.".into());
    }
    if let Some(environment) = &settings.environment {
        crate::environment::validate(environment, &settings.scene)?;
    }
    Ok(())
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Correction {
    name: String,
    black: f32,
    white: f32,
    gamma: f32,
    saturation: f32,
}

pub(crate) fn corrections(
    reader: &mut AssetReader,
) -> Result<(Vec<Correction>, Vec<String>), String> {
    let bytes = match reader.read("reflections/reflections.csv") {
        Ok(bytes) => bytes,
        Err(_) => {
            return Ok((
                vec![Correction {
                    name: "default".into(),
                    black: 0.0,
                    white: 1.0,
                    gamma: 1.0,
                    saturation: 1.0,
                }],
                vec!["Reflection correction CSV is missing. Using identity parameters.".into()],
            ))
        }
    };
    let text =
        std::str::from_utf8(&bytes).map_err(|_| "Reflection correction CSV is not UTF-8/ASCII.")?;
    let mut rows = Vec::new();
    for line in text.lines().skip(1).filter(|line| !line.trim().is_empty()) {
        let cells: Vec<_> = line
            .split(',')
            .map(|s| s.trim().trim_matches('"'))
            .collect();
        if cells.len() != 5 || !valid_name(cells[0]) || cells[0].is_empty() {
            return Err(
                "Unsupported reflection CSV row; expected a name and four finite numbers.".into(),
            );
        }
        let values: Vec<f32> = cells[1..]
            .iter()
            .map(|s| s.parse::<f32>())
            .collect::<Result<_, _>>()
            .map_err(|_| "Invalid reflection correction number.")?;
        if values.iter().any(|v| !v.is_finite() || v.abs() > 16.0) || values[2] <= 0.0 {
            return Err(
                "Reflection CSV values exceed the supported finite range (gamma must be positive)."
                    .into(),
            );
        }
        if rows
            .iter()
            .any(|r: &Correction| r.name.eq_ignore_ascii_case(cells[0]))
        {
            return Err("Reflection CSV contains duplicate names.".into());
        }
        rows.push(Correction {
            name: cells[0].into(),
            black: values[0],
            white: values[1],
            gamma: values[2],
            saturation: values[3],
        });
    }
    if rows.is_empty() || rows.len() > 256 {
        return Err("Reflection CSV needs 1–256 presets.".into());
    }
    Ok((rows, Vec::new()))
}

pub(crate) fn defaults() -> Result<ProbeSettings, String> {
    let profile = scene_profile::active()?;
    let settings = ProbeSettings {
        id: format!("{}-probes", profile.id),
        scene: profile.id.clone(),
        global_correction: profile.global_correction.clone(),
        probes: profile.probes.clone(),
        environment: None,
    };
    validate_probes(&settings)?;
    Ok(settings)
}

pub(crate) fn bake_with_entities(
    game: &Path,
    jobs: &Path,
    settings: &ProbeSettings,
    cancel: &AtomicBool,
    progress: &dyn Fn(String),
) -> Result<BakedScene, String> {
    validate_probes(settings)?;
    let mut world = match settings
        .environment
        .as_ref()
        .and_then(|e| e.worldspawn.as_ref())
    {
        Some(world) => world.clone(),
        None => worldspawn::defaults()?,
    };
    world
        .properties
        .retain(|p| p[0] != "reflection_color_correction");
    world.properties.push([
        "reflection_color_correction".into(),
        settings.global_correction.clone(),
    ]);
    worldspawn::validate_worldspawn(&world)?;
    let mut source = crate::entities::source_map(&world)?;
    if let Some(environment) = &settings.environment {
        source = crate::environment::apply_light_edits(&source, &environment.lights)?;
    }
    for probe in &settings.probes {
        source.push_str(&format!("{{\n\"classname\" \"reflection_probe\"\n\"origin\" \"{} {} {}\"\n\"color_correction\" \"{}\"\n}}\n", probe.origin[0], probe.origin[1], probe.origin[2], probe.correction));
    }
    let (mut scene, bytes) =
        bake::bake_entities_source(game, jobs, settings, &source, cancel, progress)?;
    let count = u32_at(&bytes, 8)? as usize;
    let mut cursor = 12 + count * 8;
    let mut compiled = Vec::new();
    for i in 0..count {
        let kind = u32_at(&bytes, 12 + i * 8)?;
        let size = u32_at(&bytes, 16 + i * 8)? as usize;
        if kind == 41 {
            let table = bytes
                .get(cursor..cursor + size)
                .ok_or("Truncated probe table.")?;
            if size % 0x20044 != 0 {
                return Err("Invalid probe table size.".into());
            }
            for (i, record) in table.chunks_exact(0x20044).enumerate() {
                if record[76..].iter().any(|v| *v != 0) {
                    return Err("The compiler returned nonblack probe inputs. First-generation capture requires fresh placeholders.".into());
                }
                compiled.push(CompiledProbe {
                    index: i + 1,
                    origin: [f32_at(record, 0)?, f32_at(record, 4)?, f32_at(record, 8)?],
                    correction: string_at(record, 12)?,
                });
            }
        }
        cursor += (size + 3) & !3;
    }
    if compiled.len() != settings.probes.len()
        || scene
            .manifest
            .draws
            .iter()
            .any(|d| d.probe_index == 0 || d.probe_index > compiled.len())
    {
        return Err("Compiled probe count or surface assignments are invalid. Place probes inside the enclosed scene.".into());
    }
    for (actual, authored) in compiled.iter().zip(&settings.probes) {
        if actual.origin != authored.origin {
            return Err("The compiler changed probe order or origins; capture stopped.".into());
        }
    }
    if cancel.load(Ordering::Acquire) {
        return Err("Bake cancelled.".into());
    }
    let mut reader = AssetReader::new_entities(game)?;
    let mut buffers = std::collections::HashMap::new();
    let sky_model = world.get("skyboxmodel").unwrap_or("");
    scene.manifest.sky = if sky_model.is_empty() {
        None
    } else {
        Some(
            serde_json::to_value(sky::load(&mut reader, sky_model, &mut buffers)?)
                .map_err(|e| e.to_string())?,
        )
    };
    scene.manifest.worldspawn = Some(world);
    scene.manifest.probes = Some(compiled);
    scene
        .manifest
        .warnings
        .retain(|s| !s.starts_with("Architectural opaque"));
    if let Some(instances) = scene
        .manifest
        .models
        .as_mut()
        .and_then(|m| m["instances"].as_array_mut())
    {
        for instance in instances {
            instance["probeIndex"] = serde_json::json!(1);
        }
    }
    scene.manifest.warnings.push("First-generation reflection capture: fixed architecture, placed models and sky; one probe with black input seed and gamma 0.9. Fog, Vision and recursive captures are excluded. No scripts or gameplay are executed.".into());
    scene.buffers.extend(buffers);
    Ok(scene)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn bounded_probe_text_and_positions() {
        let mut s = defaults().unwrap();
        assert!(validate_probes(&s).is_ok());
        s.probes[0].correction = "\"\nentity".into();
        assert!(validate_probes(&s).is_err());
        s.probes[0].correction.clear();
        s.probes[0].origin[0] = f32::NAN;
        assert!(validate_probes(&s).is_err());
    }
}
