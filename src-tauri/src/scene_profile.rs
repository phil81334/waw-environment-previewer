//! Scene files selected by scenes/active.json and embedded at build time.
use crate::{level_fx::LevelEffect, Probe};
use serde::Deserialize;
use std::sync::OnceLock;

pub const SCENE_MAP: &str = include_str!(concat!(env!("OUT_DIR"), "/scene.map"));
pub const PROFILE_JSON: &str = include_str!(concat!(env!("OUT_DIR"), "/profile.json"));
pub const FOLDER: &str = env!("WAW_SCENE_FOLDER");

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SceneProfile {
    pub id: String,
    pub default_sky: String,
    pub global_correction: String,
    pub probes: Vec<Probe>,
    pub fx: Vec<LevelEffect>,
}

pub fn active() -> Result<&'static SceneProfile, String> {
    static PROFILE: OnceLock<Result<SceneProfile, String>> = OnceLock::new();
    PROFILE.get_or_init(|| {
        let profile: SceneProfile = serde_json::from_str(PROFILE_JSON)
            .map_err(|e| format!("Invalid scenes/{FOLDER}/profile.json: {e}"))?;
        if profile.id.is_empty() || profile.id.len() > 48 || !profile.id.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'-') {
            return Err("The scene ID must contain 1–48 letters, digits or hyphens.".into());
        }
        if profile.probes.len() != 1 || profile.fx.len() > 1 {
            return Err("The scene profile supports one reflection probe and zero or one initial FX placement.".into());
        }
        if !profile.default_sky.is_empty() {
            crate::assets::name(&profile.default_sky)?;
        }
        for fx in &profile.fx {
            if fx.id.is_empty() || fx.name.is_empty() || fx.name.len() > 240 || fx.name.split('/').any(|part| part.is_empty()
                || !part.bytes().all(|byte| byte.is_ascii_alphanumeric() || b"_-".contains(&byte))) ||
                !fx.origin.iter().chain(&fx.angles).all(|n| n.is_finite() && n.abs() < 16_777_216.0) {
                return Err("Invalid FX asset name, identifier or transform in the scene profile.".into());
            }
        }
        Ok(profile)
    }).as_ref().map_err(Clone::clone)
}
