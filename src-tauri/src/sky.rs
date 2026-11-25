//! Bounded stock v25 rigid model and named-sky reader. Assets remain external.
use crate::assets::{f32_at, name, string_at, u16_at, u32_at, AssetReader};
use crate::{Texture, TextureBinding};
use serde::Serialize;
use std::collections::{HashMap, HashSet};

struct RigidVertex {
    pub position: [f32; 3],
    pub color: [u8; 4],
    pub uv: [f32; 2],
}
struct RigidSurface {
    pub material: String,
    pub vertices: Vec<RigidVertex>,
    pub indices: Vec<u16>,
}

struct Cursor<'a> {
    bytes: &'a [u8],
    position: usize,
}
impl<'a> Cursor<'a> {
    fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, position: 0 }
    }
    fn skip(&mut self, count: usize) -> Result<(), String> {
        self.position = self
            .position
            .checked_add(count)
            .filter(|n| *n <= self.bytes.len())
            .ok_or("Truncated rigid model.")?;
        Ok(())
    }
    fn word(&mut self) -> Result<u16, String> {
        let value = u16_at(self.bytes, self.position)?;
        self.skip(2)?;
        Ok(value)
    }
    fn integer(&mut self) -> Result<u32, String> {
        let value = u32_at(self.bytes, self.position)?;
        self.skip(4)?;
        Ok(value)
    }
    fn number(&mut self) -> Result<f32, String> {
        let value = f32_at(self.bytes, self.position)?;
        self.skip(4)?;
        Ok(value)
    }
    fn vector(&mut self) -> Result<[f32; 3], String> {
        Ok([self.number()?, self.number()?, self.number()?])
    }
    fn text(&mut self) -> Result<String, String> {
        let value = string_at(self.bytes, self.position)?;
        self.skip(value.len() + 1)?;
        Ok(value)
    }
}

/// Highest detail rigid surfaces only; skeletal/weighted models are explicit errors.
fn read_rigid_model(reader: &mut AssetReader, model: &str) -> Result<Vec<RigidSurface>, String> {
    name(model)?;
    let data = reader.read(&format!("xmodel/{model}"))?;
    let mut c = Cursor::new(&data);
    if c.word()? != 25 {
        return Err(format!("Unsupported xmodel version: {model}"));
    }
    c.skip(25)?;
    c.text()?;
    c.text()?;
    let mut lods = Vec::new();
    for _ in 0..4 {
        c.number()?;
        let lod = c.text()?;
        if !lod.is_empty() {
            name(&lod)?;
            lods.push(lod);
        }
    }
    if lods.is_empty() {
        return Err(format!("Model has no render surfaces: {model}"));
    }
    c.skip(4)?;
    let collisions = c.integer()? as usize;
    if collisions > 65536 {
        return Err("Unsupported model collision count.".into());
    }
    for _ in 0..collisions {
        let count = c.integer()? as usize;
        c.skip(
            count
                .checked_mul(48)
                .and_then(|n| n.checked_add(36))
                .ok_or("Invalid model collision size.")?,
        )?;
    }
    let mut materials = Vec::new();
    for lod in 0..lods.len() {
        let count = c.word()? as usize;
        if count > 1024 {
            return Err("Too many model materials.".into());
        }
        let mut current = Vec::new();
        for _ in 0..count {
            let material = c.text()?;
            name(&material)?;
            current.push(material);
        }
        if lod == 0 {
            materials = current;
        }
    }
    let data = reader.read(&format!("xmodelsurfs/{}", lods[0]))?;
    let mut c = Cursor::new(&data);
    if c.word()? != 25 || c.word()? as usize != materials.len() {
        return Err("Rigid surface/material table mismatch.".into());
    }
    let mut result = Vec::new();
    for material in materials {
        c.skip(3)?;
        let vertex_count = c.word()? as usize;
        let triangle_count = c.word()? as usize;
        let mut group = c.word()? as usize;
        let mut covered = 0usize;
        while group != 0 {
            if c.word()? != 0 {
                return Err(format!(
                    "Weighted or non-root rigid model is unsupported: {model}"
                ));
            }
            covered = covered
                .checked_add(group)
                .ok_or("Invalid rigid group size.")?;
            if covered > vertex_count {
                return Err("Rigid group exceeds vertex count.".into());
            }
            group = c.word()? as usize;
        }
        if vertex_count == 0 || covered != vertex_count {
            return Err("Unsupported rigid vertex groups.".into());
        }
        let mut vertices = Vec::with_capacity(vertex_count);
        for _ in 0..vertex_count {
            c.vector()?; // Normal; sky passes use position, vertex colour and UV.
            let bgra = c
                .bytes
                .get(c.position..c.position + 4)
                .ok_or("Truncated vertex colour.")?;
            let color = [bgra[2], bgra[1], bgra[0], bgra[3]];
            c.skip(4)?;
            let uv = [c.number()?, c.number()?];
            c.vector()?; // Tangent
            c.vector()?; // Binormal
            let position = c.vector()?;
            vertices.push(RigidVertex {
                position,
                color,
                uv,
            });
        }
        let mut indices = Vec::with_capacity(triangle_count * 3);
        for _ in 0..triangle_count * 3 {
            let index = c.word()?;
            if index as usize >= vertex_count {
                return Err("Model index exceeds vertex count.".into());
            }
            indices.push(index);
        }
        result.push(RigidSurface {
            material,
            vertices,
            indices,
        });
    }
    if c.position != data.len() {
        return Err("Unsupported trailing rigid surface data.".into());
    }
    Ok(result)
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct SkyLayer {
    material: String,
    techset: String,
    sort: u8,
    vertex_buffer: String,
    index_buffer: String,
    vertex_count: usize,
    index_count: usize,
    stride: usize,
    states: [u32; 2],
    scroll: [f32; 2],
    textures: Vec<TextureBinding>,
}
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct SkyScene {
    model: String,
    layers: Vec<SkyLayer>,
    textures: Vec<Texture>,
}

// Runtime packing uses IEEE binary16, then the original shader's finite-half
// decoder (including its non-IEEE zero bias). Round-to-nearest, ties-to-even.
fn half(value: f32) -> Result<u16, String> {
    let bits = value.to_bits();
    let sign = ((bits >> 16) & 0x8000) as u16;
    let exponent = ((bits >> 23) & 255) as i32 - 127 + 15;
    let mantissa = bits & 0x7fffff;
    if exponent >= 31 {
        return Err("Sky UV exceeds the original half-coordinate range.".into());
    }
    if exponent <= 0 {
        if exponent < -10 {
            return Ok(sign);
        }
        let full = mantissa | 0x800000;
        let shift = (14 - exponent) as u32;
        let base = full >> shift;
        let remainder = full & ((1 << shift) - 1);
        let midpoint = 1 << (shift - 1);
        return Ok(sign
            | (base + u32::from(remainder > midpoint || (remainder == midpoint && base & 1 != 0)))
                as u16);
    }
    let rounded = mantissa + 0xfff + ((mantissa >> 13) & 1);
    let result = ((exponent as u32) << 10) + (rounded >> 13);
    if result >= 0x7c00 {
        return Err("Sky UV packing overflow.".into());
    }
    Ok(sign | result as u16)
}

pub(crate) fn load(
    reader: &mut AssetReader,
    model: &str,
    buffers: &mut HashMap<String, Vec<u8>>,
) -> Result<SkyScene, String> {
    let surfaces = read_rigid_model(reader, model)?;
    let mut layers = Vec::new();
    let mut textures = Vec::new();
    let mut image_names = HashSet::new();
    for (index, surface) in surfaces.into_iter().enumerate() {
        let material = reader.material(&surface.material)?;
        if ![
            "sky_noncubemap",
            "sky_noncubemap_add",
            "sky_noncubemap_transition",
        ]
        .contains(&material.techset.as_str())
        {
            return Err(format!(
                "Unsupported named-sky material {} ({}).",
                surface.material, material.techset
            ));
        }
        if !material.textures.iter().any(|t| t.semantic == "colorMap")
            || material
                .textures
                .iter()
                .any(|t| !["colorMap", "colorMap01"].contains(&t.semantic.as_str()))
        {
            return Err(format!(
                "Unsupported sky texture bindings: {}",
                surface.material
            ));
        }
        if material.techset.ends_with("transition")
            && !material.textures.iter().any(|t| t.semantic == "colorMap01")
        {
            return Err("Transition sky is missing its second image.".into());
        }
        let raw = reader.read(&format!("materials/{}", surface.material))?;
        let table = u32_at(&raw, 60)? as usize;
        let count = u16_at(&raw, 50)? as usize;
        let mut scroll = [0.0; 2];
        for n in 0..count {
            let p = table + n * 20;
            if string_at(&raw, u32_at(&raw, p)? as usize)? == "uvScroll" {
                scroll = [f32_at(&raw, p + 4)?, f32_at(&raw, p + 8)?];
            }
        }
        let mut vertices = Vec::with_capacity(surface.vertices.len() * 44);
        for v in &surface.vertices {
            for value in v.position {
                vertices.extend_from_slice(&value.to_le_bytes());
            }
            for value in v.color {
                vertices.extend_from_slice(&(value as f32 / 255.0).to_le_bytes());
            }
            // Half V first, half U second: this is the original packed stream.
            for value in half(v.uv[1])?
                .to_le_bytes()
                .into_iter()
                .chain(half(v.uv[0])?.to_le_bytes())
            {
                vertices.extend_from_slice(&(value as f32).to_le_bytes());
            }
        }
        let mut indices = Vec::with_capacity(surface.indices.len() * 2);
        for value in &surface.indices {
            indices.extend_from_slice(&value.to_le_bytes());
        }
        let vertex_buffer = format!("sky-{model}-{index}-vertices");
        let index_buffer = format!("sky-{model}-{index}-indices");
        buffers.insert(vertex_buffer.clone(), vertices);
        buffers.insert(index_buffer.clone(), indices);
        for binding in &material.textures {
            if image_names.insert(binding.texture_id.clone()) {
                textures.push(reader.texture(&binding.texture_id, buffers)?);
            }
        }
        layers.push(SkyLayer {
            material: surface.material,
            techset: material.techset,
            sort: *raw.get(9).ok_or("Truncated material sort key.")?,
            vertex_buffer,
            index_buffer,
            vertex_count: surface.vertices.len(),
            index_count: surface.indices.len(),
            stride: 44,
            states: material.states,
            scroll,
            textures: material.textures,
        });
    }
    layers.sort_by_key(|layer| layer.sort);
    Ok(SkyScene {
        model: model.to_owned(),
        layers,
        textures,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn original_half_packing() {
        assert_eq!(half(0.0).unwrap(), 0);
        assert_eq!(half(1.0).unwrap(), 0x3c00);
        assert_eq!(half(-2.0).unwrap(), 0xc000);
        assert_eq!(half(65504.0).unwrap(), 0x7bff);
        assert!(half(70000.0).is_err());
    }
    #[test]
    #[ignore = "requires an explicitly supplied installed WaW/Modtools asset directory"]
    fn installed_stock_sky_families() {
        let root = std::env::var("WAW_SKY_ASSET_TEST_ROOT").expect("Set WAW_SKY_ASSET_TEST_ROOT");
        let mut reader = AssetReader::new(std::path::Path::new(&root)).unwrap();
        for (model, count) in [("skybox_pel1", 3), ("skybox_zombie", 5), ("skybox_fly", 2)] {
            let mut buffers = HashMap::new();
            let sky = load(&mut reader, model, &mut buffers).unwrap();
            assert_eq!(sky.layers.len(), count);
            for layer in &sky.layers {
                assert_eq!(buffers[&layer.vertex_buffer].len(), layer.vertex_count * 44);
                assert_eq!(buffers[&layer.index_buffer].len(), layer.index_count * 2);
                assert!(layer.index_count > 0);
            }
            println!(
                "{model}: {} layers, {} images, {} vertices",
                sky.layers.len(),
                sky.textures.len(),
                sky.layers
                    .iter()
                    .map(|layer| layer.vertex_count)
                    .sum::<usize>()
            );
        }
        assert!(load(&mut reader, "../bad", &mut HashMap::new()).is_err());
        let directory = std::path::Path::new(&root).join("raw/xmodel");
        let discovered = reader.installed_sky_models().unwrap();
        for entry in std::fs::read_dir(directory).unwrap().filter_map(Result::ok) {
            let model = entry.file_name().to_string_lossy().into_owned();
            if !model.starts_with("skybox_") || !entry.path().is_file() {
                continue;
            }
            assert!(
                discovered.contains(&model),
                "Skybox missing from picker: {model}"
            );
        }
        for model in &discovered {
            load(&mut reader, model, &mut HashMap::new())
                .unwrap_or_else(|error| panic!("Installed sky {model}: {error}"));
        }
        println!(
            "Loaded all {} installed skybox models in the picker.",
            discovered.len()
        );
    }
}
