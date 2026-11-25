//! Placed loose-v25 meshes for the Entities preview. This reads geometry and
//! authored/compiled transforms only; it never executes entity scripts.
//! Descriptor and rest-mesh layout adapted from the GPL-3.0 WaW Model Viewer.
use crate::assets::{self, AssetReader};
use crate::{bake, scene, BakedScene, Bounds, Material, SceneObject};
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::Path;

struct Reader<'a> {
    bytes: &'a [u8],
    cursor: usize,
}
impl<'a> Reader<'a> {
    fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, cursor: 0 }
    }
    fn bytes(&mut self, count: usize) -> Result<&'a [u8], String> {
        let end = self
            .cursor
            .checked_add(count)
            .ok_or("Model offset overflow.")?;
        let value = self
            .bytes
            .get(self.cursor..end)
            .ok_or("Truncated model data.")?;
        self.cursor = end;
        Ok(value)
    }
    fn u8(&mut self) -> Result<u8, String> {
        Ok(self.bytes(1)?[0])
    }
    fn u16(&mut self) -> Result<u16, String> {
        Ok(u16::from_le_bytes(self.bytes(2)?.try_into().unwrap()))
    }
    fn u32(&mut self) -> Result<u32, String> {
        Ok(u32::from_le_bytes(self.bytes(4)?.try_into().unwrap()))
    }
    fn float(&mut self) -> Result<f32, String> {
        let value = f32::from_le_bytes(self.bytes(4)?.try_into().unwrap());
        if !value.is_finite() {
            return Err("A model attribute is not finite.".into());
        }
        Ok(value)
    }
    fn vector(&mut self) -> Result<[f32; 3], String> {
        Ok([self.float()?, self.float()?, self.float()?])
    }
    fn string(&mut self) -> Result<String, String> {
        let value = assets::string_at(self.bytes, self.cursor)?;
        self.cursor += value.len() + 1;
        Ok(value)
    }
    fn version(&mut self) -> Result<(), String> {
        if self.u16()? != 25 {
            return Err("Only loose PC WaW v25 model data is supported.".into());
        }
        Ok(())
    }
}

struct Lod {
    slot: usize,
    name: String,
    materials: Vec<String>,
}
fn descriptor(bytes: &[u8]) -> Result<Vec<Lod>, String> {
    let mut reader = Reader::new(bytes);
    reader.version()?;
    reader.bytes(25)?;
    reader.string()?;
    reader.string()?;
    let mut lods = Vec::new();
    for slot in 0..4 {
        reader.float()?;
        let name = reader.string()?;
        if !name.is_empty() {
            assets::name(&name)?;
            lods.push(Lod {
                slot,
                name,
                materials: Vec::new(),
            });
        }
    }
    reader.bytes(4)?;
    let groups = reader.u32()?;
    if groups > 65536 {
        return Err("Invalid model collision group count.".into());
    }
    for _ in 0..groups {
        let count = reader.u32()? as usize;
        let length = count
            .checked_mul(48)
            .and_then(|n| n.checked_add(36))
            .ok_or("Model collision size overflow.")?;
        reader.bytes(length)?;
    }
    for lod in &mut lods {
        let count = reader.u16()?;
        if count > 4096 {
            return Err("Invalid model material count.".into());
        }
        for _ in 0..count {
            let material = reader.string()?;
            assets::name(&material)?;
            lod.materials.push(material);
        }
    }
    if lods.is_empty() {
        return Err("The model has no renderable LOD reference.".into());
    }
    Ok(lods)
}

struct Mesh {
    vertices: Vec<u8>,
    indices: Vec<u8>,
    bounds: Bounds,
}
fn meshes(bytes: &[u8]) -> Result<Vec<Mesh>, String> {
    let mut reader = Reader::new(bytes);
    reader.version()?;
    let count = reader.u16()?;
    if count > 4096 {
        return Err("Invalid model surface count.".into());
    }
    let mut output = Vec::new();
    for _ in 0..count {
        reader.bytes(3)?;
        let vertices = reader.u16()? as usize;
        let triangles = reader.u16()? as usize;
        let mut groups = Vec::new();
        let mut grouped = 0usize;
        loop {
            let count = reader.u16()? as usize;
            if count == 0 {
                break;
            }
            grouped += count;
            if grouped > vertices || groups.len() > 255 {
                return Err("Invalid rigid model vertex groups.".into());
            }
            groups.push((count, reader.u16()?));
        }
        let rigid = groups.len() == 1 && grouped == vertices;
        if !rigid {
            reader.bytes(2)?;
        }
        let mut packed = Vec::with_capacity(vertices * 52);
        let mut bounds = Bounds {
            min: [f32::INFINITY; 3],
            max: [f32::NEG_INFINITY; 3],
        };
        for _ in 0..vertices {
            let normal = reader.vector()?;
            let bgra: [u8; 4] = reader.bytes(4)?.try_into().unwrap();
            let uv = [reader.float()?, reader.float()?];
            // Preserve the actual tangent basis; the standalone colour viewer
            // can skip these fields, but scene normal/specular lighting cannot.
            let tangent = reader.vector()?;
            let binormal = reader.vector()?;
            let extra = if rigid { 0 } else { reader.u8()? };
            if extra > 3 {
                return Err("Unsupported model vertex influence count.".into());
            }
            if !rigid {
                reader.u16()?;
            }
            let position = reader.vector()?;
            let mut weight_sum = 0u32;
            for _ in 0..extra {
                reader.u16()?;
                weight_sum += u32::from(reader.u16()?);
            }
            if weight_sum > 65535 {
                return Err("Invalid model vertex weights.".into());
            }
            let cross = [
                normal[1] * tangent[2] - normal[2] * tangent[1],
                normal[2] * tangent[0] - normal[0] * tangent[2],
                normal[0] * tangent[1] - normal[1] * tangent[0],
            ];
            let sign = if cross.iter().zip(binormal).map(|(a, b)| a * b).sum::<f32>() >= 0.0 {
                1.0f32
            } else {
                -1.0f32
            };
            for value in position
                .into_iter()
                .chain(normal)
                .chain(tangent)
                .chain([sign])
                .chain(uv)
            {
                packed.extend_from_slice(&value.to_le_bytes());
            }
            packed.extend_from_slice(&[bgra[2], bgra[1], bgra[0], bgra[3]]);
            for axis in 0..3 {
                bounds.min[axis] = bounds.min[axis].min(position[axis]);
                bounds.max[axis] = bounds.max[axis].max(position[axis]);
            }
        }
        let mut indices = Vec::with_capacity(triangles * 12);
        for _ in 0..triangles * 3 {
            let index = reader.u16()?;
            if usize::from(index) >= vertices {
                return Err("Model triangle references a missing vertex.".into());
            }
            // Retain native clockwise order, matching the shared scene renderer.
            indices.extend_from_slice(&u32::from(index).to_le_bytes());
        }
        output.push(Mesh {
            vertices: packed,
            indices,
            bounds,
        });
    }
    if reader.cursor != bytes.len() {
        return Err("Unexpected trailing model surface data.".into());
    }
    Ok(output)
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Surface {
    vertex_buffer: String,
    index_buffer: String,
    vertex_count: usize,
    index_count: usize,
    stride: usize,
    index_type: &'static str,
    material: Material,
    bounds: Bounds,
}
#[derive(Serialize)]
struct Asset {
    id: String,
    name: String,
    lod: usize,
    surfaces: Vec<Surface>,
    bounds: Bounds,
}

fn load_asset(
    name: &str,
    assets: &mut AssetReader,
    scene: &mut BakedScene,
) -> Result<Asset, String> {
    assets::name(name)?;
    let lods = descriptor(&assets.read(&format!("xmodel/{name}"))?)?;
    let lod = &lods[0];
    let meshes = meshes(&assets.read(&format!("xmodelsurfs/{}", lod.name))?)?;
    if meshes.len() != lod.materials.len() {
        return Err("Model surfaces and material slots do not agree.".into());
    }
    let mut surfaces = Vec::new();
    let mut bounds = Bounds {
        min: [f32::INFINITY; 3],
        max: [f32::NEG_INFINITY; 3],
    };
    for (index, (mesh, material_name)) in meshes.into_iter().zip(&lod.materials).enumerate() {
        if mesh.indices.is_empty() {
            continue;
        }
        let material = match assets.material(material_name) {
            Ok(value) => value,
            Err(error) => {
                scene
                    .manifest
                    .warnings
                    .push(format!("Model {name} surface {index} skipped: {error}"));
                continue;
            }
        };
        let mut ready = true;
        for binding in &material.textures {
            if scene
                .manifest
                .textures
                .iter()
                .any(|texture| texture.id == binding.texture_id)
            {
                continue;
            }
            let mut buffers = HashMap::new();
            match assets.texture(&binding.texture_id, &mut buffers) {
                Ok(mut texture) => {
                    let qualified = format!("{}/{}", scene.manifest.id, texture.buffer);
                    let bytes = buffers
                        .remove(&texture.buffer)
                        .ok_or("Model texture buffer is missing.")?;
                    scene.buffers.insert(qualified.clone(), bytes);
                    texture.buffer = qualified;
                    scene.manifest.textures.push(texture);
                }
                Err(error) => {
                    scene
                        .manifest
                        .warnings
                        .push(format!("Model {name} surface {index} skipped: {error}"));
                    ready = false;
                    break;
                }
            }
        }
        if !ready {
            continue;
        }
        let prefix = format!("{}/model-{name}-{index}", scene.manifest.id);
        let vertex_buffer = format!("{prefix}-vertices");
        let index_buffer = format!("{prefix}-indices");
        let vertex_count = mesh.vertices.len() / 52;
        let index_count = mesh.indices.len() / 4;
        scene.buffers.insert(vertex_buffer.clone(), mesh.vertices);
        scene.buffers.insert(index_buffer.clone(), mesh.indices);
        for axis in 0..3 {
            bounds.min[axis] = bounds.min[axis].min(mesh.bounds.min[axis]);
            bounds.max[axis] = bounds.max[axis].max(mesh.bounds.max[axis]);
        }
        surfaces.push(Surface {
            vertex_buffer,
            index_buffer,
            vertex_count,
            index_count,
            stride: 52,
            index_type: "uint32",
            material,
            bounds: mesh.bounds,
        });
    }
    if surfaces.is_empty() {
        return Err("No supported model surfaces could be loaded.".into());
    }
    Ok(Asset {
        id: name.into(),
        name: name.into(),
        lod: lod.slot,
        surfaces,
        bounds,
    })
}

fn vector_property(
    entity: &BTreeMap<String, String>,
    name: &str,
    default: [f32; 3],
) -> Result<[f32; 3], String> {
    let Some(text) = entity.get(name) else {
        return Ok(default);
    };
    let values: Vec<f32> = text
        .split_whitespace()
        .map(str::parse)
        .collect::<Result<_, _>>()
        .map_err(|_| format!("Invalid model {name}."))?;
    if values.len() != 3 || values.iter().any(|value| !value.is_finite()) {
        return Err(format!("Invalid finite model {name}."));
    }
    Ok([values[0], values[1], values[2]])
}

// Same basis convention as the original static-model loader's AnglesToAxis:
// local X = forward, local Y = -right, local Z = up. Matrices are three bases.
fn axis(angles: [f32; 3]) -> [[f32; 3]; 3] {
    let [p, y, r] = angles.map(f32::to_radians);
    let (sp, cp) = p.sin_cos();
    let (sy, cy) = y.sin_cos();
    let (sr, cr) = r.sin_cos();
    [
        [cp * cy, cp * sy, -sp],
        [sr * sp * cy - cr * sy, sr * sp * sy + cr * cy, sr * cp],
        [cr * sp * cy + sr * sy, cr * sp * sy - sr * cy, cr * cp],
    ]
}
fn world_bounds(local: &Bounds, origin: [f32; 3], axis: [[f32; 3]; 3], scale: f32) -> Bounds {
    let mut result = Bounds {
        min: [f32::INFINITY; 3],
        max: [f32::NEG_INFINITY; 3],
    };
    for corner in 0..8 {
        let point: [f32; 3] = std::array::from_fn(|i| {
            if corner & (1 << i) == 0 {
                local.min[i]
            } else {
                local.max[i]
            }
        });
        for i in 0..3 {
            let value = origin[i]
                + scale * (point[0] * axis[0][i] + point[1] * axis[1][i] + point[2] * axis[2][i]);
            result.min[i] = result.min[i].min(value);
            result.max[i] = result.max[i].max(value);
        }
    }
    result
}

fn entity_bytes(data: &[u8]) -> Result<Option<&[u8]>, String> {
    if data.len() < 12 || &data[..4] != b"IBSP" || assets::u32_at(data, 4)? != 31 {
        return Err("Placed models require a valid BSP31 input.".into());
    }
    let count = assets::u32_at(data, 8)? as usize;
    if !(1..=100).contains(&count) {
        return Err("Invalid model BSP directory size.".into());
    }
    let mut cursor = 12usize
        .checked_add(count * 8)
        .ok_or("Model BSP directory overflow.")?;
    let mut entities = None;
    for index in 0..count {
        let kind = assets::u32_at(data, 12 + index * 8)?;
        let size = assets::u32_at(data, 16 + index * 8)? as usize;
        let end = cursor.checked_add(size).ok_or("Model BSP lump overflow.")?;
        let bytes = data.get(cursor..end).ok_or("Truncated model BSP lump.")?;
        if kind == 39 && entities.replace(bytes).is_some() {
            return Err("Duplicate compiled entity lump.".into());
        }
        cursor = cursor
            .checked_add((size + 3) & !3)
            .ok_or("Model BSP padding overflow.")?;
    }
    if cursor != data.len() {
        return Err("Invalid model BSP trailing data.".into());
    }
    Ok(entities)
}
fn placement_entities(
    data: &[u8],
    warnings: &mut Vec<String>,
) -> Result<Vec<BTreeMap<String, String>>, String> {
    let Some(bytes) = entity_bytes(data)? else {
        warnings.push("Placed models omitted: no compiled entity text; other supported scene content retained.".into());
        return Ok(Vec::new());
    };
    match scene::compiled_entities(bytes) {
        Ok(entities) => Ok(entities),
        Err(error) => {
            warnings.push(format!("Placed models omitted: unreadable compiled entity text: {error}; other supported scene content retained."));
            Ok(Vec::new())
        }
    }
}

/// Authored model placement only. Dynamic classes are displayed in their saved rest pose;
/// physics, damage and gameplay scripts are deliberately not simulated.
pub(crate) fn is_placed_model_class(classname: &str) -> bool {
    ["misc_model", "script_model", "dyn_model"].contains(&classname)
}

pub(crate) fn append_models(
    data: &[u8],
    assets: &mut AssetReader,
    baked: &mut BakedScene,
) -> Result<(), String> {
    let entities = placement_entities(data, &mut baked.manifest.warnings)?;
    let mut loaded: BTreeMap<String, Asset> = BTreeMap::new();
    let mut failed: BTreeMap<String, String> = BTreeMap::new();
    let mut instances = Vec::new();
    let mut skipped = Vec::new();
    for (source_index, entity) in entities.iter().enumerate() {
        let classname = entity.get("classname").map(String::as_str).unwrap_or("");
        if !is_placed_model_class(classname) {
            continue;
        }
        let model = entity.get("model").cloned().unwrap_or_default();
        let id = format!("model-{source_index}");
        let result = (|| -> Result<_, String> {
            let origin = vector_property(entity, "origin", [0.0; 3])?;
            let mut angles = vector_property(entity, "angles", [0.0; 3])?;
            if !entity.contains_key("angles") {
                if let Some(value) = entity.get("angle") {
                    angles[1] = value.parse::<f32>().map_err(|_| "Invalid model angle.")?;
                    if !angles[1].is_finite() {
                        return Err("Invalid finite model angle.".into());
                    }
                }
            }
            let scale = entity
                .get("modelscale")
                .map(|text| text.parse::<f32>())
                .transpose()
                .map_err(|_| "Invalid model scale.")?
                .unwrap_or(1.0);
            if !scale.is_finite() || scale <= 0.0 {
                return Err("Model scale must be finite and positive.".into());
            }
            if let Some(error) = failed.get(&model) {
                return Err(error.clone());
            }
            if !loaded.contains_key(&model) {
                match load_asset(&model, assets, baked) {
                    Ok(asset) => {
                        loaded.insert(model.clone(), asset);
                    }
                    Err(error) => {
                        failed.insert(model.clone(), error.clone());
                        return Err(error);
                    }
                }
            }
            let asset = loaded.get(&model).unwrap();
            let axis = axis(angles);
            let bounds = world_bounds(&asset.bounds, origin, axis, scale);
            if bounds
                .min
                .iter()
                .chain(&bounds.max)
                .any(|value| !value.is_finite())
            {
                return Err("Model transformed bounds overflow.".into());
            }
            let ground = entity
                .get("gndLt")
                .and_then(|text| parse_ground_lighting(text));
            let primary_light_index = ground
                .map(|(_, primary)| primary as usize)
                .filter(|index| *index < baked.manifest.lights.len())
                .unwrap_or(0);
            let ground_json = ground.map(|(rgba, _)| serde_json::json!({"present":true,"rgb":[rgba[0] as f32/255.0,rgba[1] as f32/255.0,rgba[2] as f32/255.0],"alpha":rgba[3] as f32/255.0,"packed":u32::from_le_bytes([rgba[2],rgba[1],rgba[0],rgba[3]])})).unwrap_or_else(|| serde_json::json!({"present":false,"rgb":[0,0,0],"alpha":0,"packed":0}));
            let label = entity
                .get("targetname")
                .cloned()
                .unwrap_or_else(|| model.clone());
            baked
                .manifest
                .objects
                .get_or_insert_with(Vec::new)
                .push(SceneObject {
                    id: id.clone(),
                    kind: if classname == "misc_model" {
                        "staticModel"
                    } else {
                        "model"
                    }
                    .into(),
                    classname: classname.into(),
                    model: model.clone(),
                    origin,
                    angles,
                    scale,
                    bounds: bounds.clone(),
                    draw_count: asset.surfaces.len(),
                });
            for i in 0..3 {
                baked.manifest.bounds.min[i] = baked.manifest.bounds.min[i].min(bounds.min[i]);
                baked.manifest.bounds.max[i] = baked.manifest.bounds.max[i].max(bounds.max[i]);
            }
            Ok(
                serde_json::json!({"id":id,"assetId":model,"sourceIndex":source_index,"classname":classname,"label":label,"model":model,"origin":origin,"angles":angles,"axis":axis,"scale":scale,"bounds":bounds,"groundLighting":ground_json,"primaryLightIndex":primary_light_index,"probeIndex":0}),
            )
        })();
        match result {
            Ok(instance) => instances.push(instance),
            Err(error) => {
                baked
                    .manifest
                    .warnings
                    .push(format!("Placed model {model} ({id}) skipped: {error}"));
                skipped.push(serde_json::json!({"id":id,"sourceIndex":source_index,"classname":classname,"model":model,"reason":error}));
            }
        }
    }
    baked.manifest.models = Some(
        serde_json::json!({"assets":loaded.into_values().collect::<Vec<_>>(),"instances":instances,"skipped":skipped}),
    );
    Ok(())
}

fn parse_ground_lighting(text: &str) -> Option<([u8; 4], u8)> {
    // Modern BSP31 serialized RRGGBBAAPP; original parser stores D3DCOLOR BGRA.
    // Keep logical RGBA here. Missing/legacy values require a separately proved
    // fallback and therefore are not silently treated as current valid lighting.
    if text.len() != 10 || !text.is_ascii() {
        return None;
    }
    let mut values = [0u8; 5];
    for (index, value) in values.iter_mut().enumerate() {
        *value = u8::from_str_radix(&text[index * 2..index * 2 + 2], 16).ok()?;
    }
    Some(([values[0], values[1], values[2], values[3]], values[4]))
}

fn source_model_names(map: &str) -> Result<BTreeSet<String>, String> {
    let mut names = BTreeSet::new();
    for entity in bake::top_entities(map)? {
        let mut properties = BTreeMap::new();
        let mut depth = 0i32;
        for line in entity.lines().map(str::trim) {
            if line.starts_with("//") {
                continue;
            }
            if line == "{" {
                depth += 1;
                continue;
            }
            if line == "}" {
                depth -= 1;
                continue;
            }
            if depth != 1 || !line.starts_with('"') {
                continue;
            }
            let strings: Vec<_> = line.split('"').collect();
            if strings.len() >= 5 {
                properties.insert(strings[1], strings[3]);
            }
        }
        if properties
            .get("classname")
            .is_some_and(|classname| is_placed_model_class(classname))
        {
            if let Some(model) = properties.get("model") {
                assets::name(model)?;
                names.insert((*model).into());
            }
        }
    }
    Ok(names)
}

pub(crate) fn prepare_compiler_assets(
    map: &str,
    assets: &mut AssetReader,
    raw: &Path,
) -> Result<Vec<String>, String> {
    let mut copied = BTreeSet::new();
    let mut copy = |relative: &str, assets: &mut AssetReader| -> Result<Vec<u8>, String> {
        copied.insert(relative.to_owned());
        assets.copy_to(relative, raw)
    };
    for model in source_model_names(map)? {
        let bytes = copy(&format!("xmodel/{model}"), assets)?;
        let lods = descriptor(&bytes)?;
        // Parts belong to the primary rest pose. Lower LODs commonly do not
        // have separate xmodelparts files; all LOD surface streams are copied.
        copy(&format!("xmodelparts/{}", lods[0].name), assets)?;
        for lod in lods {
            copy(&format!("xmodelsurfs/{}", lod.name), assets)?;
            for name in lod.materials {
                copy(&format!("materials/{name}"), assets)?;
                let material = assets.material(&name)?;
                for binding in material.textures {
                    copy(&format!("images/{}.iwi", binding.texture_id), assets)?;
                }
            }
        }
    }
    Ok(copied.into_iter().collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn ground_lighting_matches_original_parser_reference_cases() {
        let (rgba, primary) = parse_ground_lighting("1312110001").unwrap();
        assert_eq!(rgba, [0x13, 0x12, 0x11, 0]);
        assert_eq!(primary, 1);
        assert_eq!(
            u32::from_le_bytes([rgba[2], rgba[1], rgba[0], rgba[3]]),
            0x00131211
        );
        assert_eq!(
            parse_ground_lighting("112233440A"),
            Some(([0x11, 0x22, 0x33, 0x44], 10))
        );
        assert!(parse_ground_lighting("11223344").is_none());
        assert!(parse_ground_lighting("zzzzzzzzzz").is_none());
    }
    #[test]
    fn source_model_dependencies_ignore_prefab_and_actor_helpers() {
        let source = "iwmap 4\n{\n\"classname\" \"misc_model\"\n\"model\" \"crate\"\n}\n{\n\"classname\" \"script_model\"\n\"model\" \"door\"\n}\n{\n\"classname\" \"misc_prefab\"\n\"model\" \"_prefabs/traverse.map\"\n}\n{\n\"classname\" \"actor_zombie\"\n\"model\" \"actor\"\n}\n";
        assert_eq!(
            source_model_names(source)
                .unwrap()
                .into_iter()
                .collect::<Vec<_>>(),
            ["crate", "door"]
        );
    }
    #[test]
    fn model_basis_matches_original_angle_to_axis_controls() {
        // Recorded by executing only original AnglesToAxis 0x55433D privately.
        let cases = [
            (
                [0.0, 0.0, 0.0],
                [1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0],
            ),
            (
                [0.0, 90.0, 0.0],
                [0.0, 1.0, 0.0, -1.0, 0.0, 0.0, 0.0, 0.0, 1.0],
            ),
            (
                [90.0, 0.0, 0.0],
                [0.0, 0.0, -1.0, 0.0, 1.0, 0.0, 1.0, 0.0, 0.0],
            ),
            (
                [0.0, 0.0, 90.0],
                [1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, -1.0, 0.0],
            ),
            (
                [10.0, 20.0, 30.0],
                [
                    0.925416529,
                    0.336824059,
                    -0.173648179,
                    -0.214610144,
                    0.843493223,
                    0.492403865,
                    0.312324524,
                    -0.41841206,
                    0.852868497,
                ],
            ),
        ];
        for (angles, expected) in cases {
            for (actual, value) in axis(angles).into_iter().flatten().zip(expected) {
                assert!((actual - value).abs() < 0.000001);
            }
        }
    }

    #[test]
    fn authored_rotation_scale_and_world_bounds_are_correct() {
        let basis = axis([0.0, 90.0, 0.0]);
        let bounds = world_bounds(
            &Bounds {
                min: [1.0, 2.0, 3.0],
                max: [4.0, 5.0, 6.0],
            },
            [10.0, 20.0, 30.0],
            basis,
            2.0,
        );
        for (actual, expected) in bounds
            .min
            .into_iter()
            .zip([0.0, 22.0, 36.0])
            .chain(bounds.max.into_iter().zip([6.0, 28.0, 42.0]))
        {
            assert!((actual - expected).abs() < 0.00001);
        }
    }
    #[test]
    fn authored_dynamic_models_are_meshes_but_brush_entities_are_not_xmodels() {
        let source = "{\n\"classname\" \"dyn_model\"\n\"model\" \"clutter_bucket\"\n}\n{\n\"classname\" \"dyn_brushmodel\"\n\"model\" \"*2\"\n}\n";
        assert_eq!(
            source_model_names(source).unwrap(),
            BTreeSet::from(["clutter_bucket".to_owned()])
        );
        assert!(is_placed_model_class("dyn_model"));
        assert!(!is_placed_model_class("dyn_brushmodel"));
        assert!(!is_placed_model_class("actor_zombie"));
    }
    #[test]
    fn optional_entity_text_cannot_abort_other_supported_content() {
        let bsp = |kind: u32, payload: &[u8]| {
            let mut data = b"IBSP".to_vec();
            data.extend_from_slice(&31_u32.to_le_bytes());
            data.extend_from_slice(&1_u32.to_le_bytes());
            data.extend_from_slice(&kind.to_le_bytes());
            data.extend_from_slice(&(payload.len() as u32).to_le_bytes());
            data.extend_from_slice(payload);
            while data.len() % 4 != 0 {
                data.push(0);
            }
            data
        };
        let mut warnings = Vec::new();
        assert!(placement_entities(&bsp(0, &[]), &mut warnings)
            .unwrap()
            .is_empty());
        assert!(warnings[0].contains("no compiled entity text"));
        warnings.clear();
        assert!(placement_entities(&bsp(39, b"{ malformed"), &mut warnings)
            .unwrap()
            .is_empty());
        assert!(warnings[0].contains("unreadable compiled entity text"));
        let mut malformed = bsp(39, b"{ malformed");
        malformed.truncate(21);
        assert!(placement_entities(&malformed, &mut warnings).is_err());
    }
}
