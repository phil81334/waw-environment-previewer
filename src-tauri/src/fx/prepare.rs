//! Read authored scene FX dependencies and pack geometry for the shared D3D9 viewport.

use std::collections::{BTreeSet, VecDeque};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use base64::{engine::general_purpose::STANDARD, Engine};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use super::{cloud_geometry, loader, model_assets, model_pose, renderer};

type Result<T> = std::result::Result<T, String>;
pub(crate) const MAX_CLIP_BYTES: usize = 128 * 1024 * 1024;
const PREVIEW_SEED: u32 = 17;

fn load_named(game: &Path, path: &Path) -> Result<Value> {
    let mut effect = loader::load(path)?;
    let relative = path
        .strip_prefix(game.join("raw/fx"))
        .map_err(|error| error.to_string())?;
    effect["name"] = json!(canonical_effect_name(&relative.to_string_lossy())?);
    Ok(effect)
}

/// Resolve all active visual dependencies and compile without advancing time.
/// A successful preflight is not a completed render certificate.
pub(crate) fn preflight_typed(
    game: &Path,
    name: &str,
) -> std::result::Result<Value, crate::scene_fx::FxPrepareError> {
    let name = canonical_effect_name(name)?;
    let mut effect = load_named(game, &game.join("raw/fx").join(&name).with_extension("efx"))?;
    effect["previewPackedLighting"] = json!(0x4210_u16);
    resolve_dependencies(game, &mut effect)?;
    let compiled = super::simulate(&effect, PREVIEW_SEED, &[])?;
    let has_visual = compiled["compiled"]
        .as_array()
        .into_iter()
        .flatten()
        .chain(
            compiled["dependencyCompiled"]
                .as_object()
                .into_iter()
                .flat_map(|definitions| definitions.values())
                .filter_map(Value::as_array)
                .flatten(),
        )
        .any(|element| {
            element["kind"]
                .as_str()
                .is_some_and(|kind| !["sound", "decal", "runner"].contains(&kind))
        });
    if !has_visual {
        return Err(crate::scene_fx::FxPrepareError::capability(
            "no_visual_elements",
            "This effect has no visual elements to preview",
        ));
    }
    resolve_materials_with_model_reader(game, &mut effect, |game, name| {
        renderer::model_material_info(game, name).map_err(|error| error.to_string())
    })?;
    super::simulate(&effect, PREVIEW_SEED, &[])?;
    renderer::validate_material_manifest(
        &material_manifest(&effect)?,
        &effect["sharedMaterialInfo"],
    )
    .map_err(|error| error.to_string())?;
    if super::memory_cost(&effect) > MAX_CLIP_BYTES / 2 {
        return Err("This effect's assets exceed the preview memory limit".into());
    }
    attach_fingerprint(game, &mut effect)?;
    Ok(effect)
}

/// Load every graph material before playing, including children that have not
/// spawned yet. These are asset names, not captured or invented draw geometry.
pub(crate) fn material_manifest(source: &Value) -> Result<Vec<Value>> {
    let mut draws = Vec::new();
    for definition in std::iter::once(source).chain(
        source["dependencies"]
            .as_object()
            .into_iter()
            .flat_map(|items| items.values()),
    ) {
        for element in definition["elements"]
            .as_array()
            .ok_or("Missing elements")?
        {
            if loader::words(element, "editorFlags")
                .iter()
                .any(|flag| flag == "disabled")
            {
                continue;
            }
            for field in [
                "billboardSprite",
                "orientedSprite",
                "tail",
                "line",
                "cloud",
                "trail",
            ] {
                for material in loader::words(element, field) {
                    if material.is_empty() || ["{", "}", ";"].contains(&material.as_str()) {
                        continue;
                    }
                    let format = if field == "cloud" {
                        "cloud20"
                    } else {
                        "sprite32"
                    };
                    draws.push(json!({"texture":material,"vertexFormat":format}));
                }
            }
        }
    }
    if let Some(models) = source["modelInfo"].as_object() {
        for model in models.values() {
            for surface in model["surfaces"]
                .as_array()
                .ok_or("Missing model surfaces")?
            {
                let material = surface["material"]
                    .as_str()
                    .ok_or("Missing model material")?;
                draws.push(
                    json!({"texture":format!("__model__/{material}"),"vertexFormat":"model32"}),
                );
            }
        }
    }
    Ok(vec![json!({"draws":draws})])
}

fn attach_fingerprint(game: &Path, effect: &mut Value) -> Result<()> {
    let mut paths = BTreeSet::<PathBuf>::new();
    paths.insert(
        game.join("raw/fx")
            .join(effect["name"].as_str().ok_or("Missing effect name")?)
            .with_extension("efx"),
    );
    if let Some(definitions) = effect["dependencies"].as_object() {
        for name in definitions.keys() {
            paths.insert(game.join("raw/fx").join(name).with_extension("efx"));
        }
    }
    if let Some(models) = effect["modelInfo"].as_object() {
        for (name, model) in models {
            paths.insert(model_assets::asset_path(game, "xmodel", name)?);
            let lod = model["lodName"].as_str().ok_or("Missing model LOD name")?;
            paths.insert(model_assets::asset_path(game, "xmodelsurfs", lod)?);
            paths.insert(model_assets::asset_path(game, "xmodelparts", lod)?);
            for surface in model["surfaces"]
                .as_array()
                .ok_or("Missing model surfaces")?
            {
                let name = surface["material"]
                    .as_str()
                    .ok_or("Missing model material")?;
                let info =
                    renderer::model_material_info(game, name).map_err(|error| error.to_string())?;
                material_paths(game, name, &info, true, &mut paths)?;
            }
        }
    }
    if let Some(materials) = effect["sharedMaterialInfo"].as_object() {
        for (name, info) in materials {
            // Model materials use mc_ techniques; their exact files were
            // recorded with the model above, not as sprite FX materials.
            if !name.starts_with("__model__/") {
                material_paths(game, name, info, false, &mut paths)?;
            }
        }
    }
    for entry in fs::read_dir(game.join("main")).map_err(|error| error.to_string())? {
        let path = entry.map_err(|error| error.to_string())?.path();
        if path
            .extension()
            .is_some_and(|extension| extension.eq_ignore_ascii_case("iwd"))
        {
            paths.insert(path);
        }
    }
    let mut records = Vec::new();
    for path in paths {
        let metadata =
            fs::metadata(&path).map_err(|error| format!("Read {}: {error}", path.display()))?;
        let modified = metadata
            .modified()
            .map_err(|error| error.to_string())?
            .duration_since(UNIX_EPOCH)
            .map_err(|error| error.to_string())?
            .as_nanos()
            .to_string();
        records.push(
            json!({"path":path.strip_prefix(game).map_err(|error|error.to_string())?
            .to_string_lossy().replace('\\',"/"),"size":metadata.len(),"modified":modified}),
        );
    }
    let bytes = serde_json::to_vec(&records).map_err(|error| error.to_string())?;
    effect["dependencyFingerprint"] = json!(format!("{:x}", Sha256::digest(bytes)));
    effect["dependencyFiles"] = json!(records);
    Ok(())
}

fn material_paths(
    game: &Path,
    name: &str,
    info: &Value,
    model: bool,
    paths: &mut BTreeSet<PathBuf>,
) -> Result<()> {
    let name = name.strip_prefix("__model__/").unwrap_or(name);
    paths.insert(game.join("raw/materials").join(name));
    let family = info["family"].as_str().ok_or("Missing material family")?;
    let prefix = if model { "mc_" } else { "" };
    paths.insert(
        game.join("raw/techsets")
            .join(format!("{prefix}{family}.techset")),
    );
    for field in ["pass", "authoredPass", "effectivePass"] {
        if let Some(pass) = info[field].as_str() {
            paths.insert(game.join("raw/techniques").join(format!("{pass}.tech")));
        }
    }
    if let Some(map) = info["stateMap"].as_str() {
        paths.insert(game.join("raw/statemaps").join(format!("{map}.sm")));
    }
    for field in ["vertexProgram", "pixelProgram"] {
        if let Some(program) = info[field].as_str() {
            paths.insert(game.join("raw/shader_bin").join(program));
        }
    }
    Ok(())
}

fn canonical_effect_name(value: &str) -> Result<String> {
    let mut value = value.trim_matches('"').replace('\\', "/");
    if value.to_ascii_lowercase().ends_with(".efx") {
        value.truncate(value.len() - 4);
    }
    if value.is_empty()
        || value.starts_with('/')
        || value.contains(':')
        || value.split('/').any(|part| part == ".." || part.is_empty())
    {
        return Err(format!("Invalid child effect name: {value}").into());
    }
    Ok(value.to_ascii_lowercase())
}

fn child_names(effect: &Value) -> Result<Vec<String>> {
    let mut names = BTreeSet::new();
    for element in effect["elements"].as_array().ok_or("Missing elements")? {
        if loader::words(element, "editorFlags")
            .iter()
            .any(|word| word == "disabled")
        {
            continue;
        }
        let editor_flags = loader::words(element, "editorFlags");
        // Native preview spawn exits before consulting these types' children.
        if element.get("decal").is_some() || element.get("sound").is_some() {
            continue;
        }
        // Native conversion nulls these authored references when their editor
        // flag is disabled. A dormant name must not demand files or materials.
        let references = [
            ("runner", true),
            (
                "fxOnDeath",
                editor_flags.iter().any(|flag| flag == "playOnDeath"),
            ),
            // The fixed empty-scene preview uses EffectsEd's original no-hit
            // trace policy. Preserve authored impact metadata in the compiler,
            // but do not load assets reachable only through a collision.
            ("fxOnImpact", false),
            (
                "emission",
                editor_flags.iter().any(|flag| flag == "playOnRun"),
            ),
        ];
        for (field, enabled) in references {
            if !enabled {
                continue;
            }
            for word in loader::words(element, field) {
                if !word.is_empty() && !["{", "}", ";"].contains(&word.as_str()) {
                    names.insert(canonical_effect_name(&word)?);
                }
            }
        }
    }
    Ok(names.into_iter().collect())
}

/// Load a finite definition registry without recursively embedding JSON trees.
/// A visited set makes cyclic references finite to read; runtime expansion must
/// still enforce its separate recursion/particle/effect limits.
fn resolve_dependencies(game: &Path, effect: &mut Value) -> Result<()> {
    let root = canonical_effect_name(effect["name"].as_str().ok_or("Missing effect name")?)?;
    effect["name"] = json!(root);
    let mut visited = BTreeSet::from([root.clone()]);
    let mut queue: VecDeque<String> = child_names(effect)?.into();
    let mut definitions = serde_json::Map::new();
    while let Some(child) = queue.pop_front() {
        if !visited.insert(child.clone()) {
            continue;
        }
        if visited.len() > 512 {
            return Err("Effect dependency graph exceeds 512 definitions".into());
        }
        let path = game.join("raw/fx").join(&child).with_extension("efx");
        let mut definition = load_named(game, &path)
            .map_err(|error| format!("Dependency {child} of {root}: {error}"))?;
        definition["name"] = json!(child);
        queue.extend(child_names(&definition)?);
        definitions.insert(child, definition);
    }
    effect["dependencies"] = Value::Object(definitions);
    resolve_copy_sort_metadata(game, effect)?;
    Ok(())
}

/// Native conversion copies each distinct authored emission child's one-shot
/// class even when playOnRun is off. Those copies affect sort averages, but do
/// not start extra particles. Read their metadata without requiring render
/// assets for dormant children or recursively expanding inactive dependencies.
fn copied_sort_orders(game: &Path, effect: &Value) -> Result<Value> {
    let mut seen = BTreeSet::new();
    let mut orders = Vec::new();
    for element in effect["elements"].as_array().ok_or("Missing elements")? {
        if loader::words(element, "editorFlags")
            .iter()
            .any(|flag| flag == "disabled")
        {
            continue;
        }
        for reference in loader::words(element, "emission") {
            if reference.is_empty() {
                continue;
            }
            let name = canonical_effect_name(&reference)?;
            if !seen.insert(name.clone()) {
                continue;
            }
            let path = game.join("raw/fx").join(&name).with_extension("efx");
            let child = load_named(game, &path)
                .map_err(|error| format!("Emitted-class sort metadata {name}: {error}"))?;
            for copied in child["elements"]
                .as_array()
                .ok_or("Missing child elements")?
            {
                let flags = loader::words(copied, "editorFlags");
                if flags
                    .iter()
                    .any(|flag| flag == "disabled" || flag == "looping")
                    || copied.get("runner").is_some()
                {
                    continue;
                }
                let value = loader::words(copied, "sortOrder")
                    .first()
                    .map(|word| word.parse::<i64>())
                    .transpose()
                    .map_err(|error| error.to_string())?
                    .unwrap_or(5);
                orders.push(value.clamp(0, 255) as u8);
            }
        }
    }
    Ok(json!(orders))
}

fn resolve_copy_sort_metadata(game: &Path, effect: &mut Value) -> Result<()> {
    effect["copySortOrders"] = copied_sort_orders(game, effect)?;
    if let Some(definitions) = effect["dependencies"].as_object_mut() {
        for child in definitions.values_mut() {
            child["copySortOrders"] = copied_sort_orders(game, child)?;
        }
    }
    Ok(())
}

/// Resolve every required visual dependency before accepting whole-effect play.
/// Sound names are not material references and never request an audio service.

fn resolve_materials_with_model_reader(
    game: &Path,
    effect: &mut Value,
    model_reader: fn(&Path, &str) -> Result<Value>,
) -> Result<()> {
    let mut model_info = serde_json::Map::new();
    let mut model_materials = serde_json::Map::new();
    for source in std::iter::once(&*effect).chain(
        effect["dependencies"]
            .as_object()
            .into_iter()
            .flat_map(|definitions| definitions.values()),
    ) {
        for element in source["elements"]
            .as_array()
            .ok_or("Missing model elements")?
        {
            if loader::words(element, "editorFlags")
                .iter()
                .any(|flag| flag == "disabled")
            {
                continue;
            }
            for name in loader::words(element, "model") {
                if name.is_empty()
                    || ["{", "}", ";"].contains(&name.as_str())
                    || model_info.contains_key(&name)
                {
                    continue;
                }
                let model = model_assets::read(game, &name)
                    .map_err(|error| format!("Model {name}: {error}"))?;
                for surface in &model.surfaces {
                    let key = format!("__model__/{}", surface.material);
                    if !model_materials.contains_key(&key) {
                        let metadata = model_reader(game, &surface.material).map_err(|error| {
                            format!("Model {name}, material {}: {error}", surface.material)
                        })?;
                        model_materials.insert(key, metadata);
                    }
                }
                model_info.insert(
                    name,
                    serde_json::to_value(model).map_err(|error| error.to_string())?,
                );
            }
        }
    }
    if !model_info.is_empty() {
        effect["modelInfo"] = Value::Object(model_info);
    }
    let own = renderer::material_info(game, effect).map_err(|error| error.to_string())?;
    let mut shared = own.as_object().ok_or("Invalid material map")?.clone();
    shared.extend(model_materials);
    effect["materialInfo"] = own;
    if let Some(definitions) = effect["dependencies"].as_object_mut() {
        for (name, definition) in definitions {
            let information = renderer::material_info(game, definition)
                .map_err(|error| format!("Material dependency in {name}: {error}"))?;
            for (name, value) in information
                .as_object()
                .ok_or("Invalid child material map")?
            {
                shared.insert(name.clone(), value.clone());
            }
            definition["materialInfo"] = information;
        }
    }
    effect["sharedMaterialInfo"] = Value::Object(shared);
    Ok(())
}

/// Static model and cloud geometry is packed once per loaded effect.
pub(crate) struct FrameProjection {
    models: serde_json::Map<String, Value>,
    cloud_vertices: String,
    cloud_indices: Value,
}

impl FrameProjection {
    pub(crate) fn new(model_info: &Value) -> Result<Self> {
        let mut models = serde_json::Map::new();
        if let Some(assets) = model_info.as_object() {
            for (name, asset) in assets {
                let mut surfaces = Vec::new();
                for surface in asset["surfaces"]
                    .as_array()
                    .ok_or("Missing model surface data")?
                {
                    let bytes = model_pose::pack_surface_json(&surface["geometry"])?;
                    surfaces.push(json!({
                        "texture":format!("__model__/{}", surface["material"].as_str().ok_or("Missing model material")?),
                        "vertexFormat":"model32", "vertices":STANDARD.encode(bytes),
                        "indices":surface["geometry"]["indices"]
                    }));
                }
                models.insert(name.clone(), json!(surfaces));
            }
        }
        let (cloud_vertices, cloud_indices) = cloud_geometry::static_mesh(1);
        Ok(Self {
            models,
            cloud_vertices: STANDARD.encode(cloud_vertices),
            cloud_indices: json!(cloud_indices),
        })
    }

    /// Apply the current level camera, including its actual viewport aspect.
    pub(crate) fn project(
        &self,
        frame: &mut Value,
        origin: [f32; 3],
        rows: [[f32; 4]; 4],
        view: [[f32; 4]; 4],
        projection: [[f32; 4]; 4],
    ) -> Result<()> {
        if !origin
            .iter()
            .chain(rows.iter().flatten())
            .chain(view.iter().flatten())
            .chain(projection.iter().flatten())
            .all(|value| value.is_finite())
        {
            return Err("Nonfinite scene FX camera.".into());
        }
        let view_axes = std::array::from_fn(|axis| [view[axis][0], view[axis][1], view[axis][2]]);
        frame["matrixRows"] = json!(rows);
        frame["viewMatrixRows"] = json!(view);
        frame["projectionMatrixRows"] = json!(projection);
        frame["cameraOrigin"] = json!(origin);
        if let Some(models) = frame["models"].as_array() {
            let mut meshes = Vec::new();
            let mut retained_mesh_bytes = super::memory_cost(frame);
            for model in models {
                let name = model["model"].as_str().ok_or("Missing scene model name")?;
                let surfaces = self
                    .models
                    .get(name)
                    .and_then(Value::as_array)
                    .ok_or("Missing prepared model surface data")?;
                for surface in surfaces {
                    retained_mesh_bytes = retained_mesh_bytes
                        .saturating_add(super::memory_cost(surface))
                        .saturating_add(super::memory_cost(model));
                    if retained_mesh_bytes > MAX_CLIP_BYTES {
                        return Err(
                            "This effect's model instances exceed the 128 MiB frame memory limit"
                                .into(),
                        );
                    }
                    let mut mesh = surface.clone();
                    mesh["modelPlacement"] = model.clone();
                    meshes.push(mesh);
                }
            }
            meshes.extend(
                frame["draws"]
                    .as_array()
                    .ok_or("Missing FX draws")?
                    .iter()
                    .cloned(),
            );
            frame["draws"] = json!(meshes);
        }
        for draw in frame["draws"].as_array_mut().ok_or("Missing frame draws")? {
            if let Some(cloud) = draw.get("cloud").cloned() {
                let origin = super::array(&cloud["origin"], "cloud origin")?;
                let end = super::array(&cloud["endpos"], "cloud endpoint")?;
                let radius = super::array(&cloud["radius"], "cloud radii")?;
                let quat = super::array(&cloud["quat"], "cloud quaternion")?;
                let scale = cloud["scale"]
                    .as_f64()
                    .filter(|value| value.is_finite())
                    .ok_or("Missing finite cloud scale")? as f32;
                let bytes = cloud["bgra"].as_array().ok_or("Missing cloud colour")?;
                if bytes.len() != 4 {
                    return Err("Cloud colour must contain four bytes".into());
                }
                let mut color = [0_u8; 4];
                for (index, value) in bytes.iter().enumerate() {
                    color[index] = u8::try_from(value.as_u64().ok_or("Invalid cloud colour byte")?)
                        .map_err(|error| error.to_string())?;
                }
                draw["vertexFormat"] = json!("cloud20");
                draw["vertices"] = json!(self.cloud_vertices);
                draw["indices"] = self.cloud_indices.clone();
                draw["cloudMatrix"] = json!(cloud_geometry::matrix(origin, end, radius, view_axes));
                draw["cloudColor"] = json!(cloud_geometry::color(color));
                draw["worldViewMatrixRows"] =
                    json!(cloud_geometry::world_view_rows(quat, origin, scale, view));
            }
        }
        if super::memory_cost(frame) > MAX_CLIP_BYTES {
            return Err("This effect exceeds the 128 MiB preview memory limit".into());
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cloud_uses_current_viewport_camera() {
        let projector = FrameProjection::new(&Value::Null).unwrap();
        let identity = [
            [1., 0., 0., 0.],
            [0., 1., 0., 0.],
            [0., 0., 1., 0.],
            [0., 0., 0., 1.],
        ];
        let mut view = identity;
        view[0] = [0., -1., 0., 0.];
        view[1] = [1., 0., 0., 0.];
        let mut frame = json!({"draws": [{"cloud": {
            "origin": [0,0,0], "endpos": [1,0,0], "radius": [1,2],
            "quat": [0,0,0,1], "scale": 1, "bgra": [255,255,255,255]
        }}]});
        projector
            .project(&mut frame, [8., 16., 32.], identity, view, identity)
            .unwrap();
        assert_eq!(frame["viewMatrixRows"], json!(view));
        assert_eq!(frame["cameraOrigin"], json!([8., 16., 32.]));
        assert_eq!(frame["draws"][0]["cloudMatrix"], json!([1., 0., 0., 2.]));
    }
}
