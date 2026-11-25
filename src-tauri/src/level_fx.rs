//! Optional initial FX for the active scene. Placements are authored in its profile.
use crate::{
    scene_fx::{prepare_effect, EffectPlacement, PreparedEffect},
    scene_profile,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{path::Path, sync::OnceLock};

#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SourceRecord {
    pub path: String,
    pub sha256: String,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LevelEffect {
    pub id: String,
    pub name: String,
    pub origin: [f32; 3],
    pub angles: [f32; 3],
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub evidence: String,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LevelFxDocument {
    pub source_map: String,
    pub sources: Vec<SourceRecord>,
    pub placements: Vec<LevelEffect>,
    pub warnings: Vec<String>,
}

#[derive(Clone)]
pub struct PreparedLevelFx {
    pub document: LevelFxDocument,
    pub effect: Option<PreparedEffect>,
    pub report: serde_json::Value,
}

/// The map digest binds saved bakes to the exact layout in this executable.
pub fn discover() -> Result<LevelFxDocument, String> {
    static DOCUMENT: OnceLock<Result<LevelFxDocument, String>> = OnceLock::new();
    DOCUMENT
        .get_or_init(|| {
            let profile = scene_profile::active()?;
            let sources = [
                ("scene.map", scene_profile::SCENE_MAP),
                ("profile.json", scene_profile::PROFILE_JSON),
            ]
            .into_iter()
            .map(|(name, content)| SourceRecord {
                path: format!("scenes/{}/{name}", scene_profile::FOLDER),
                sha256: format!("{:x}", Sha256::digest(content.as_bytes())),
            })
            .collect();
            Ok(LevelFxDocument {
                source_map: format!("scenes/{}/scene.map", scene_profile::FOLDER),
                sources,
                placements: profile.fx.clone(),
                warnings: Vec::new(),
            })
        })
        .clone()
}

pub fn prepare(game: &Path, document: &LevelFxDocument) -> Result<PreparedLevelFx, String> {
    let mut warnings = document.warnings.clone();
    let mut placements = Vec::new();
    let mut effect = None;
    for placement in &document.placements {
        let mut row = serde_json::to_value(placement).map_err(|e| e.to_string())?;
        match prepare_effect(
            game,
            &placement.name,
            EffectPlacement {
                origin: placement.origin,
                angles: placement.angles,
            },
            1,
        ) {
            Ok(prepared) => {
                row["status"] = "loaded".into();
                row["limits"] = prepared.metadata()["limits"].clone();
                warnings.extend(
                    prepared.metadata()["warnings"]
                        .as_array()
                        .into_iter()
                        .flatten()
                        .filter_map(|v| v.as_str())
                        .map(str::to_owned),
                );
                effect = Some(prepared);
            }
            Err(error) => {
                row["status"] = "unsupported".into();
                row["warning"] = error.clone().into();
                warnings.push(error);
            }
        }
        placements.push(row);
    }
    let report = serde_json::json!({"sourceMap": document.source_map, "sources": document.sources,
        "placements": placements, "totalCount": document.placements.len(), "loadedCount": usize::from(effect.is_some()),
        "warnings": warnings, "scope": "Static scene FX placements from profile.json"});
    Ok(PreparedLevelFx {
        document: document.clone(),
        effect,
        report,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cache_identity_covers_both_embedded_inputs() {
        let document = discover().unwrap();
        assert_eq!(document.sources.len(), 2);
        assert_eq!(
            document.sources[0].sha256,
            format!("{:x}", Sha256::digest(scene_profile::SCENE_MAP.as_bytes()))
        );
        assert_eq!(
            document.sources[1].sha256,
            format!(
                "{:x}",
                Sha256::digest(scene_profile::PROFILE_JSON.as_bytes())
            )
        );
        assert_eq!(
            document.placements.len(),
            scene_profile::active().unwrap().fx.len()
        );
    }
}
