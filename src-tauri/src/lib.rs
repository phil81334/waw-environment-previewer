//! Scene compilation, installed asset readers and Direct3D 9 rendering for the previewer.
mod assets;
mod bake;
#[cfg(windows)]
pub mod d3d9;
mod entities;
mod environment;
#[allow(dead_code)]
mod fx;
pub mod level_fx;
mod lightgrid;
mod models;
mod probes;
mod scene;
pub mod scene_fx;
pub mod scene_profile;
mod sky;
mod vision;
mod worldspawn;

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

pub use entities::{bake_entities, inspect_entities};
pub use environment::{AuthoredLightEdit, EnvironmentSettings};
pub use probes::{validate_probes, CompiledProbe, Probe, ProbeSettings};
pub use vision::{vision_assets, VisionMask};
pub use worldspawn::{validate_worldspawn, WorldspawnSettings};

pub const DEFINITIONS: &[&str] = &[
    "candle",
    "light_dynamic",
    "light_no_falloff",
    "light_point_linear",
    "light_point_linear_drk_lght_drk",
    "light_point_linear_fire_fill",
    "light_point_linear_nocenter",
    "light_point_quadratic",
    "light_point_quadratic_glow",
    "light_point_quadratic_obtuse",
    "light_point_sharp_fall",
    "tungsten_lamp",
];

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Geometry {
    pub vertex_buffer: String,
    pub index_buffer: String,
    pub vertex_count: usize,
    pub index_count: usize,
    pub stride: usize,
    pub index_type: String,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Draw {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub object_id: Option<String>,
    pub first_index: usize,
    pub index_count: usize,
    pub material_index: usize,
    pub primary_light_index: usize,
    pub lightmap_index: usize,
    pub probe_index: usize,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TextureMip {
    pub offset: usize,
    pub length: usize,
    pub width: u32,
    pub height: u32,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Texture {
    pub id: String,
    pub name: String,
    pub buffer: String,
    pub format: String,
    pub width: u32,
    pub height: u32,
    pub mips: Vec<TextureMip>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TextureBinding {
    pub semantic: String,
    pub texture_id: String,
    pub flags: u32,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Material {
    pub name: String,
    pub techset: String,
    pub textures: Vec<TextureBinding>,
    pub env_map_parms: [f32; 4],
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail_scale: Option<[f32; 4]>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub color_tint: Option<[f32; 4]>,
    pub states: [u32; 2],
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Lightmap {
    pub secondary_buffer: String,
    pub width: u32,
    pub height: u32,
    pub format: String,
    pub primary_buffer: String,
    pub primary_width: u32,
    pub primary_height: u32,
    pub primary_format: String,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Light {
    pub index: usize,
    pub r#type: u8,
    pub shadow_eligible: bool,
    pub rgb: [f32; 3],
    pub direction: [f32; 3],
    pub origin: [f32; 3],
    pub radius: f32,
    pub cos_outer: f32,
    pub cos_inner: f32,
    pub exponent: u8,
    pub cos_max_turn: f32,
    pub max_move: f32,
    pub def_name: String,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Falloff {
    pub definition: String,
    pub texture_id: String,
    pub sampler: u8,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Bounds {
    pub min: [f32; 3],
    pub max: [f32; 3],
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SceneObject {
    pub id: String,
    pub kind: String,
    pub classname: String,
    pub model: String,
    pub origin: [f32; 3],
    pub angles: [f32; 3],
    pub scale: f32,
    pub bounds: Bounds,
    pub draw_count: usize,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SceneManifest {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub objects: Option<Vec<SceneObject>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub models: Option<serde_json::Value>,
    pub schema_version: u32,
    pub id: String,
    pub scene: String,
    pub source_hash: String,
    pub geometry: Geometry,
    pub draws: Vec<Draw>,
    pub materials: Vec<Material>,
    pub textures: Vec<Texture>,
    pub lightmaps: Vec<Lightmap>,
    pub lights: Vec<Light>,
    pub falloffs: Vec<Falloff>,
    pub bounds: Bounds,
    pub warnings: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub worldspawn: Option<WorldspawnSettings>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sky: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub probes: Option<Vec<CompiledProbe>>,
}
#[derive(Debug)]
pub struct BakedScene {
    pub manifest: SceneManifest,
    pub buffers: HashMap<String, Vec<u8>>,
    /// Installed prefab sources actually staged for this bake, keyed relative to map_source.
    pub prefab_hashes: std::collections::BTreeMap<String, String>,
}
