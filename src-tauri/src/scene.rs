use crate::assets::{self, f32_at, string_at, u16_at, u32_at, AssetReader};
use crate::{
    BakedScene, Bounds, Draw, Falloff, Geometry, Light, Lightmap, SceneManifest, SceneObject,
    DEFINITIONS,
};
use std::collections::{BTreeMap, HashMap, HashSet};

fn utility(name: &str) -> bool {
    [
        "caulk",
        "clip",
        "clip_missile",
        "clip_player",
        "global_invisible",
        "lightgrid_volume",
        "traverse",
        "sky_pel1",
        "sky_rhi2",
        "$default",
    ]
    .contains(&name)
}
fn vec3(bytes: &[u8], offset: usize) -> Result<[f32; 3], String> {
    Ok([
        f32_at(bytes, offset)?,
        f32_at(bytes, offset + 4)?,
        f32_at(bytes, offset + 8)?,
    ])
}
pub(crate) fn read_entities_bsp(
    data: &[u8],
    scene_id: &str,
    source_hash: String,
    assets: &mut AssetReader,
) -> Result<BakedScene, String> {
    if data.len() < 12 || &data[..4] != b"IBSP" || u32_at(data, 4)? != 31 {
        return Err("The compiler did not produce a supported BSP31 scene.".into());
    }
    let count = u32_at(data, 8)? as usize;
    if !(1..=100).contains(&count) {
        return Err("Invalid BSP directory size.".into());
    }
    let mut cursor = 12usize.checked_add(count * 8).ok_or("Invalid BSP size.")?;
    let mut lumps: HashMap<u32, &[u8]> = HashMap::new();
    for index in 0..count {
        let kind = u32_at(data, 12 + 8 * index)?;
        let size = u32_at(data, 16 + 8 * index)? as usize;
        let end = cursor.checked_add(size).ok_or("BSP lump overflow.")?;
        let payload = data.get(cursor..end).ok_or("Truncated BSP lump.")?;
        if lumps.insert(kind, payload).is_some() {
            return Err("Duplicate BSP lump.".into());
        }
        cursor = cursor
            .checked_add((size + 3) & !3)
            .ok_or("BSP padding overflow.")?;
    }
    if cursor != data.len() {
        return Err("BSP contains unexpected trailing bytes.".into());
    }
    let get = |kind| {
        lumps
            .get(&kind)
            .copied()
            .ok_or_else(|| format!("Missing BSP lump {kind}."))
    };
    let material_bytes = get(0)?;
    let vertices = get(10)?;
    let indices = get(11)?;
    let surface_bytes = get(9)?;
    let primary = get(43)?;
    let lightmap_bytes = get(1)?;
    let models = get(37)?;
    if material_bytes.len() % 72 != 0
        || vertices.len() % 68 != 0
        || indices.len() % 2 != 0
        || surface_bytes.len() % 28 != 0
        || primary.len() % 132 != 0
        || lightmap_bytes.is_empty()
        || lightmap_bytes.len() % 0x300000 != 0
    {
        return Err("A BSP record table has an invalid length.".into());
    }
    validate_surface_geometry(surface_bytes, vertices, indices, material_bytes.len() / 72)?;
    let mut lights = Vec::new();
    for (index, record) in primary.chunks_exact(132).enumerate() {
        lights.push(Light {
            index,
            r#type: record[0],
            shadow_eligible: record[1] != 0,
            rgb: vec3(record, 8)?,
            direction: vec3(record, 20)?,
            origin: vec3(record, 32)?,
            radius: f32_at(record, 44)?,
            cos_outer: f32_at(record, 48)?,
            cos_inner: f32_at(record, 52)?,
            exponent: record[56],
            cos_max_turn: f32_at(record, 60)?,
            max_move: f32_at(record, 64)?,
            def_name: string_at(record, 68)?,
        });
    }
    let mut buffers = HashMap::new();
    let mut packed = Vec::with_capacity(vertices.len() / 68 * 44);
    for vertex in vertices.chunks_exact(68) {
        let position = vec3(vertex, 0)?;
        let normal = vec3(vertex, 12)?;
        let tangent = vec3(vertex, 44)?;
        let binormal = vec3(vertex, 56)?;
        let cross = [
            normal[1] * tangent[2] - normal[2] * tangent[1],
            normal[2] * tangent[0] - normal[0] * tangent[2],
            normal[0] * tangent[1] - normal[1] * tangent[0],
        ];
        let sign: f32 = if cross.iter().zip(binormal).map(|(a, b)| a * b).sum::<f32>() >= 0.0 {
            1.0
        } else {
            -1.0
        };
        for value in position {
            packed.extend_from_slice(&value.to_le_bytes());
        }
        packed.extend_from_slice(&[vertex[26], vertex[25], vertex[24], vertex[27]]);
        for offset in [28, 32, 36, 40] {
            packed.extend_from_slice(&f32_at(vertex, offset)?.to_le_bytes());
        }
        for vector in [normal, tangent] {
            for value in vector {
                packed.push(((value * 127.0 + 127.5) as i32 & 255) as u8);
            }
            packed.push(63);
        }
        packed.extend_from_slice(&sign.to_le_bytes());
    }
    let mut materials = Vec::new();
    let mut material_remap = HashMap::new();
    let mut texture_names = HashSet::new();
    let mut draws = Vec::new();
    let mut global_indices = Vec::new();
    let mut bounds = Bounds {
        min: [f32::INFINITY; 3],
        max: [f32::NEG_INFINITY; 3],
    };
    let first = u16_at(models, 24)? as usize;
    let number = u16_at(models, 28)? as usize;
    if first
        .checked_add(number)
        .is_none_or(|n| n > surface_bytes.len() / 28)
    {
        return Err("BSP world surface range is invalid.".into());
    }
    let mut warnings = Vec::new();
    let mut objects = Vec::new();
    let ranges = brush_ranges(
        models,
        lumps.get(&39).copied(),
        surface_bytes.len() / 28,
        first,
        number,
        &mut objects,
        &mut warnings,
    )?;
    let mut rejected_materials = HashSet::new();
    let mut prepared_textures = HashMap::new();
    let mut transformed_vertices = HashMap::new();
    for range in &ranges {
        for surface in range.first..range.first + range.count {
            let draw = &surface_bytes[surface * 28..surface * 28 + 28];
            let material_id = u16_at(draw, 0)? as usize;
            let material_record = material_bytes
                .get(material_id * 72..material_id * 72 + 72)
                .ok_or("BSP material index is invalid.")?;
            let material_name = string_at(material_record, 0)?;
            // Sky surfaces remain in the compiler input for enclosure and sun
            // visibility, but the normal colour pass belongs to the sky model.
            let sky_surface = u32_at(material_record, 64)? & 4 != 0;
            if utility(&material_name) || sky_surface {
                continue;
            }
            if rejected_materials.contains(&material_id) {
                continue;
            }
            let material_index = if let Some(index) = material_remap.get(&material_id) {
                *index
            } else {
                let material = {
                    let supported = (|| {
                        let material = assets.material(&material_name)?;
                        validate_opaque_material(&material)?;
                        for binding in &material.textures {
                            if !prepared_textures.contains_key(&binding.texture_id) {
                                let texture = assets.texture(&binding.texture_id, &mut buffers)?;
                                prepared_textures.insert(binding.texture_id.clone(), texture);
                            }
                        }
                        Ok::<_, String>(material)
                    })();
                    match supported {
                        Ok(material) => material,
                        Err(error) => {
                            warnings.push(format!("Skipped material {material_name}: {error}"));
                            rejected_materials.insert(material_id);
                            continue;
                        }
                    }
                };
                for binding in &material.textures {
                    texture_names.insert(binding.texture_id.clone());
                }
                let index = materials.len();
                materials.push(material);
                material_remap.insert(material_id, index);
                index
            };
            let lightmap_index = draw[2] as usize;
            let primary_light_index = draw[4] as usize;
            if lightmap_index >= lightmap_bytes.len() / 0x300000
                || primary_light_index >= lights.len()
            {
                return Err("BSP draw references missing lighting data.".into());
            }
            let base_vertex = u32_at(draw, 16)? as usize;
            let vertex_count = u16_at(draw, 20)? as usize;
            let index_count = u16_at(draw, 22)? as usize;
            let start_index = u32_at(draw, 24)? as usize;
            if index_count % 3 != 0
                || base_vertex + vertex_count > vertices.len() / 68
                || (start_index + index_count) * 2 > indices.len()
            {
                return Err("BSP draw geometry is out of range.".into());
            }
            let first_index = global_indices.len() / 4;
            for index in start_index..start_index + index_count {
                let local = u16_at(indices, index * 2)? as usize;
                if local >= vertex_count {
                    return Err("BSP local vertex index is invalid.".into());
                }
                let absolute = base_vertex + local;
                let position = vec3(vertices, absolute * 68)?;
                let (output_index, position) = if let Some(object_index) = range.object_index {
                    let key = (object_index, absolute);
                    let position = transform_position(position, &objects[object_index]);
                    let output = if let Some(index) = transformed_vertices.get(&key) {
                        *index
                    } else {
                        let output = packed.len() / 44;
                        if output > u32::MAX as usize {
                            return Err("Entity vertex buffer exceeds uint32 limits.".into());
                        }
                        append_transformed_vertex(
                            &mut packed,
                            vertices,
                            absolute,
                            &objects[object_index],
                        )?;
                        transformed_vertices.insert(key, output);
                        output
                    };
                    (output, position)
                } else {
                    (absolute, position)
                };
                global_indices.extend_from_slice(&(output_index as u32).to_le_bytes());
                for axis in 0..3 {
                    bounds.min[axis] = bounds.min[axis].min(position[axis]);
                    bounds.max[axis] = bounds.max[axis].max(position[axis]);
                }
            }
            if let Some(object_index) = range.object_index {
                objects[object_index].draw_count += 1;
            }
            draws.push(Draw {
                object_id: range.object_index.map(|index| objects[index].id.clone()),
                first_index,
                index_count,
                material_index,
                primary_light_index,
                lightmap_index,
                probe_index: draw[3] as usize,
            });
        }
    }
    if draws.is_empty() || global_indices.is_empty() {
        return Err("The baked scene has no supported opaque world geometry.".into());
    }
    let mut falloffs = Vec::new();
    for definition in DEFINITIONS {
        let bytes = assets.read(&format!("lights/{definition}"))?;
        if bytes.first() != Some(&0x62) {
            return Err(format!(
                "Unsupported sampler in stock light definition {definition}."
            ));
        }
        let image = string_at(&bytes, 1)?;
        texture_names.insert(image.clone());
        falloffs.push(Falloff {
            definition: (*definition).to_owned(),
            texture_id: image,
            sampler: bytes[0],
        });
    }
    let mut names: Vec<_> = texture_names.into_iter().collect();
    names.sort();
    let mut textures = Vec::new();
    for image in names {
        textures.push(if let Some(texture) = prepared_textures.remove(&image) {
            texture
        } else {
            assets.texture(&image, &mut buffers)?
        });
    }
    let index_count = global_indices.len() / 4;
    let vertex_count = packed.len() / 44;
    buffers.insert("world-vertices".into(), packed);
    buffers.insert("world-indices".into(), global_indices);
    let mut lightmaps = Vec::new();
    for (index, atlas) in lightmap_bytes.chunks_exact(0x300000).enumerate() {
        let secondary_buffer = format!("secondary-{index}");
        let primary_buffer = format!("primary-{index}");
        buffers.insert(secondary_buffer.clone(), atlas[..0x200000].to_vec());
        buffers.insert(primary_buffer.clone(), atlas[0x200000..].to_vec());
        lightmaps.push(Lightmap {
            secondary_buffer,
            width: 512,
            height: 1024,
            format: "BGRA8".into(),
            primary_buffer,
            primary_width: 1024,
            primary_height: 1024,
            primary_format: "R8".into(),
        });
    }
    let id = assets::sha(data);
    let qualify = |name: &str| format!("{id}/{name}");
    for texture in &mut textures {
        texture.buffer = qualify(&texture.buffer);
    }
    for lightmap in &mut lightmaps {
        lightmap.primary_buffer = qualify(&lightmap.primary_buffer);
        lightmap.secondary_buffer = qualify(&lightmap.secondary_buffer);
    }
    let buffers = buffers
        .into_iter()
        .map(|(key, value)| (qualify(&key), value))
        .collect();
    Ok(BakedScene {
        prefab_hashes: Default::default(),
        manifest: SceneManifest {
            objects: Some(objects),
            models: None,
            schema_version: 1,
            id: id.clone(),
            scene: scene_id.to_owned(),
            source_hash,
            geometry: Geometry {
                vertex_buffer: qualify("world-vertices"),
                index_buffer: qualify("world-indices"),
                vertex_count,
                index_count,
                stride: 44,
                index_type: "uint32".into(),
            },
            draws,
            materials,
            textures,
            lightmaps,
            lights,
            falloffs,
            bounds,
            warnings,
            worldspawn: None,
            sky: None,
            probes: None,
        },
        buffers,
    })
}

fn validate_surface_geometry(
    surfaces: &[u8],
    vertices: &[u8],
    indices: &[u8],
    material_count: usize,
) -> Result<(), String> {
    for surface in surfaces.chunks_exact(28) {
        if u16_at(surface, 0)? as usize >= material_count {
            return Err("BSP surface material index is out of range.".into());
        }
        let base = u32_at(surface, 16)? as usize;
        let count = u16_at(surface, 20)? as usize;
        let index_count = u16_at(surface, 22)? as usize;
        let first_index = u32_at(surface, 24)? as usize;
        if index_count % 3 != 0
            || base
                .checked_add(count)
                .is_none_or(|end| end > vertices.len() / 68)
            || first_index
                .checked_add(index_count)
                .is_none_or(|end| end > indices.len() / 2)
        {
            return Err("BSP surface geometry is out of range.".into());
        }
        for index in first_index..first_index + index_count {
            if u16_at(indices, index * 2)? as usize >= count {
                return Err("BSP surface local vertex index is out of range.".into());
            }
        }
    }
    Ok(())
}

#[derive(Clone)]
struct BrushRange {
    first: usize,
    count: usize,
    object_index: Option<usize>,
}

fn validate_opaque_material(material: &crate::Material) -> Result<(), String> {
    if material.states != [0x08128812, 13] {
        return Err("Unverified opaque render state.".into());
    }
    if !["l_sm_r0c0n0s0_sco", "l_sm_r0c0n0s0"].contains(&material.techset.as_str()) {
        return Err(format!(
            "Unsupported material technique: {}",
            material.techset
        ));
    }
    for required in ["colorMap", "normalMap", "specularMap"] {
        if !material.textures.iter().any(|binding| {
            binding
                .semantic
                .strip_prefix("material.")
                .unwrap_or(&binding.semantic)
                == required
        }) {
            return Err(format!("Missing supported {required} binding."));
        }
    }
    Ok(())
}

/// Reads the bounded compiled entity text, not map geometry or executable script.
pub(crate) fn compiled_entities(bytes: &[u8]) -> Result<Vec<BTreeMap<String, String>>, String> {
    if bytes.len() > 16 * 1024 * 1024 {
        return Err("Compiled entity text exceeds its supported size.".into());
    }
    let text = std::str::from_utf8(bytes)
        .map_err(|_| "Compiled entity text is not UTF-8.")?
        .trim_end_matches('\0');
    let mut cursor = 0;
    let mut entities = Vec::new();
    while skip_entity_space(text, &mut cursor) {
        if text.as_bytes()[cursor] != b'{' {
            return Err("Invalid compiled entity opening brace.".into());
        }
        cursor += 1;
        let mut properties = BTreeMap::new();
        loop {
            if !skip_entity_space(text, &mut cursor) {
                return Err("Unterminated compiled entity.".into());
            }
            if text.as_bytes()[cursor] == b'}' {
                cursor += 1;
                break;
            }
            let name = entity_string(text, &mut cursor)?;
            if !skip_entity_space(text, &mut cursor) {
                return Err("Missing compiled entity value.".into());
            }
            let value = entity_string(text, &mut cursor)?;
            properties.insert(name, value);
        }
        entities.push(properties);
        if entities.len() > 65536 {
            return Err("Too many compiled entities.".into());
        }
    }
    Ok(entities)
}

fn skip_entity_space(text: &str, cursor: &mut usize) -> bool {
    while *cursor < text.len() && text.as_bytes()[*cursor].is_ascii_whitespace() {
        *cursor += 1;
    }
    *cursor < text.len()
}

fn entity_string(text: &str, cursor: &mut usize) -> Result<String, String> {
    if text.as_bytes().get(*cursor) != Some(&b'"') {
        return Err("Invalid compiled entity quoted string.".into());
    }
    *cursor += 1;
    let mut output = Vec::new();
    while let Some(&byte) = text.as_bytes().get(*cursor) {
        *cursor += 1;
        if byte == b'"' {
            return String::from_utf8(output)
                .map_err(|_| "Invalid compiled entity string encoding.".into());
        }
        if byte == 0 {
            return Err("Unexpected NUL in compiled entity text.".into());
        }
        if byte == b'\\' && matches!(text.as_bytes().get(*cursor), Some(b'"' | b'\\')) {
            output.push(text.as_bytes()[*cursor]);
            *cursor += 1;
        } else {
            output.push(byte);
        }
        if output.len() > 65536 {
            return Err("Compiled entity property is too large.".into());
        }
    }
    Err("Unterminated compiled entity string.".into())
}

fn property_vector(value: &str) -> Result<[f32; 3], String> {
    let values = value
        .split_whitespace()
        .map(str::parse::<f32>)
        .collect::<Result<Vec<_>, _>>()
        .map_err(|_| "Invalid entity transform number.")?;
    if values.len() != 3 || values.iter().any(|v| !v.is_finite()) {
        return Err("Entity transform must contain three finite numbers.".into());
    }
    Ok([values[0], values[1], values[2]])
}

fn brush_ranges(
    models: &[u8],
    entity_bytes: Option<&[u8]>,
    surfaces: usize,
    first: usize,
    count: usize,
    objects: &mut Vec<SceneObject>,
    warnings: &mut Vec<String>,
) -> Result<Vec<BrushRange>, String> {
    let mut ranges = vec![BrushRange {
        first,
        count,
        object_index: None,
    }];
    if models.len() < 48 || models.len() % 48 != 0 {
        return Err("Invalid BSP48-byte brush model table.".into());
    }
    let entities = compiled_entities(entity_bytes.ok_or("Missing compiled BSP entity text.")?)?;
    let mut used_models = HashSet::new();
    let mut used_surfaces: HashSet<_> = (first..first + count).collect();
    for entity in entities {
        let Some(model) = entity.get("model").filter(|value| value.starts_with('*')) else {
            continue;
        };
        let index = match model[1..].parse::<usize>() {
            Ok(index) if index > 0 && index < models.len() / 48 => index,
            _ => {
                warnings.push(format!("Skipped invalid brush model reference {model}."));
                continue;
            }
        };
        if !used_models.insert(index) {
            warnings.push(format!("Skipped duplicate brush model placement {model}."));
            continue;
        }
        let record = &models[index * 48..(index + 1) * 48];
        let first = u16_at(record, 24)? as usize;
        let count = u16_at(record, 28)? as usize;
        if first.checked_add(count).is_none_or(|end| end > surfaces) {
            return Err("Brush model surface range exceeds the BSP table.".into());
        }
        if (first..first + count).any(|surface| !used_surfaces.insert(surface)) {
            return Err("Brush model surface ranges overlap the world or another model.".into());
        }
        let origin = entity
            .get("origin")
            .ok_or("Compiled brush model has no origin.")
            .and_then(|value| property_vector(value).map_err(|_| "Invalid compiled brush origin."));
        let angles = if let Some(value) = entity.get("angles") {
            property_vector(value)
        } else if let Some(value) = entity.get("angle") {
            value
                .parse::<f32>()
                .ok()
                .filter(|value| value.is_finite())
                .map(|yaw| [0.0, yaw, 0.0])
                .ok_or_else(|| "Invalid brush yaw angle.".into())
        } else {
            Ok([0.0; 3])
        };
        let (origin, angles) = match (origin, angles) {
            (Ok(origin), Ok(angles)) => (origin, angles),
            _ => {
                warnings.push(format!(
                    "Skipped brush model {model} with invalid transform."
                ));
                continue;
            }
        };
        let local_min = vec3(record, 0)?;
        let local_max = vec3(record, 12)?;
        if (0..3).any(|axis| local_min[axis] > local_max[axis]) {
            return Err("Brush model bounds are inverted.".into());
        }
        let mut object = SceneObject {
            id: format!("brush-{index}"),
            kind: "brush".into(),
            classname: entity.get("classname").cloned().unwrap_or_default(),
            model: model.clone(),
            origin,
            angles,
            scale: 1.0,
            bounds: Bounds {
                min: [f32::INFINITY; 3],
                max: [f32::NEG_INFINITY; 3],
            },
            draw_count: 0,
        };
        for corner in 0..8 {
            let local = [0, 1, 2].map(|axis| {
                if corner & (1 << axis) == 0 {
                    local_min[axis]
                } else {
                    local_max[axis]
                }
            });
            let world = transform_position(local, &object);
            if world.iter().any(|value| !value.is_finite()) {
                return Err("Brush bounds transform overflow.".into());
            }
            for axis in 0..3 {
                object.bounds.min[axis] = object.bounds.min[axis].min(world[axis]);
                object.bounds.max[axis] = object.bounds.max[axis].max(world[axis]);
            }
        }
        let object_index = objects.len();
        objects.push(object);
        ranges.push(BrushRange {
            first,
            count,
            object_index: Some(object_index),
        });
    }
    Ok(ranges)
}

// CoD AngleVectors basis is forward, negative-right, up. The local model Y
// axis therefore points to positive world Y at identity and to -X at yaw90.
fn rotate_brush(vector: [f32; 3], angles: [f32; 3]) -> [f32; 3] {
    let (sp, cp) = angles[0].to_radians().sin_cos();
    let (sy, cy) = angles[1].to_radians().sin_cos();
    let (sr, cr) = angles[2].to_radians().sin_cos();
    let axis = [
        [cp * cy, cp * sy, -sp],
        [sr * sp * cy - cr * sy, sr * sp * sy + cr * cy, sr * cp],
        [cr * sp * cy + sr * sy, cr * sp * sy - sr * cy, cr * cp],
    ];
    [0, 1, 2]
        .map(|row| axis[0][row] * vector[0] + axis[1][row] * vector[1] + axis[2][row] * vector[2])
}

fn transform_position(position: [f32; 3], object: &SceneObject) -> [f32; 3] {
    let rotated = rotate_brush(position, object.angles);
    [0, 1, 2].map(|axis| rotated[axis] + object.origin[axis])
}

fn append_transformed_vertex(
    packed: &mut Vec<u8>,
    vertices: &[u8],
    absolute: usize,
    object: &SceneObject,
) -> Result<(), String> {
    let source = vertices
        .get(absolute * 68..absolute * 68 + 68)
        .ok_or("Invalid brush source vertex.")?;
    let packed_source = packed
        .get(absolute * 44..absolute * 44 + 44)
        .ok_or("Invalid packed brush vertex.")?
        .to_vec();
    let start = packed.len();
    packed.extend_from_slice(&packed_source);
    let position = transform_position(vec3(source, 0)?, object);
    if position.iter().any(|value| !value.is_finite()) {
        return Err("Brush vertex transform overflow.".into());
    }
    for axis in 0..3 {
        packed[start + axis * 4..start + axis * 4 + 4]
            .copy_from_slice(&position[axis].to_le_bytes());
    }
    for (input, output) in [(12, 32), (44, 36)] {
        let direction = rotate_brush(vec3(source, input)?, object.angles);
        for axis in 0..3 {
            packed[start + output + axis] = ((direction[axis] * 127.0 + 127.5) as i32 & 255) as u8;
        }
    }
    Ok(())
}

#[cfg(test)]
mod entity_tests {
    use super::*;

    #[test]
    fn compiled_entity_text_is_bounded_and_preserves_quoted_metadata() {
        let entities = compiled_entities(
            br#"{ "classname" "script_brushmodel" "model" "*3" "origin" "1 2 3" }"#,
        )
        .unwrap();
        assert_eq!(entities[0]["model"], "*3");
        assert_eq!(
            property_vector(&entities[0]["origin"]).unwrap(),
            [1.0, 2.0, 3.0]
        );
        assert!(compiled_entities(br#"{ "classname" "unfinished"#).is_err());
        assert!(compiled_entities(b"{ classname unquoted }").is_err());
        assert!(property_vector("NaN 0 0").is_err());
    }

    #[test]
    fn entity_basis_matches_independent_axis_rotation_controls() {
        let check = |actual: [f32; 3], expected: [f32; 3]| {
            assert!(actual
                .iter()
                .zip(expected)
                .all(|(a, b)| (a - b).abs() < 0.00001));
        };
        check(
            rotate_brush([1.0, 0.0, 0.0], [0.0, 90.0, 0.0]),
            [0.0, 1.0, 0.0],
        );
        check(
            rotate_brush([0.0, 1.0, 0.0], [0.0, 90.0, 0.0]),
            [-1.0, 0.0, 0.0],
        );
        check(
            rotate_brush([1.0, 0.0, 0.0], [90.0, 0.0, 0.0]),
            [0.0, 0.0, -1.0],
        );
        check(
            rotate_brush([0.0, 1.0, 0.0], [0.0, 0.0, 90.0]),
            [0.0, 0.0, 1.0],
        );
    }

    #[test]
    fn malformed_submodel_ranges_do_not_duplicate_world_geometry() {
        let mut models = vec![0; 96];
        models[48 + 28..48 + 30].copy_from_slice(&1_u16.to_le_bytes());
        let text = br#"{ "classname" "script_brushmodel" "model" "*1" "origin" "0 0 0" }"#;
        assert!(brush_ranges(
            &models,
            Some(text),
            1,
            0,
            1,
            &mut Vec::new(),
            &mut Vec::new()
        )
        .is_err());
        models[48 + 24..48 + 26].copy_from_slice(&1_u16.to_le_bytes());
        assert!(brush_ranges(
            &models,
            Some(text),
            1,
            0,
            1,
            &mut Vec::new(),
            &mut Vec::new()
        )
        .is_err());
    }
}
