//! Read-only effect preparation for a frozen compiled scene.
use crate::fx;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{path::Path, sync::Arc};

#[derive(Clone, Copy, Debug, Deserialize, Serialize)]
pub struct EffectPlacement {
    pub origin: [f32; 3],
    pub angles: [f32; 3],
}

#[derive(Clone, Debug, Serialize, PartialEq)]
#[serde(tag = "status", rename_all = "lowercase")]
pub enum FxPrepareError {
    Omitted {
        code: &'static str,
        detail: String,
        evidence: Option<Value>,
    },
    Fatal {
        code: &'static str,
        detail: String,
        evidence: Option<Value>,
    },
}
impl FxPrepareError {
    pub fn capability(code: &'static str, detail: impl Into<String>) -> Self {
        Self::Omitted {
            code,
            detail: detail.into(),
            evidence: None,
        }
    }
    pub fn detail(&self) -> &str {
        match self {
            Self::Omitted { detail, .. } | Self::Fatal { detail, .. } => detail,
        }
    }
}
impl From<String> for FxPrepareError {
    fn from(detail: String) -> Self {
        Self::Fatal {
            code: "unclassified_native_failure",
            detail,
            evidence: None,
        }
    }
}
impl From<&str> for FxPrepareError {
    fn from(detail: &str) -> Self {
        detail.to_string().into()
    }
}
impl std::fmt::Display for FxPrepareError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.detail())
    }
}

#[derive(Clone)]
pub struct PreparedEffect {
    pub name: String,
    pub seed: u32,
    pub placement: EffectPlacement,
    pub(crate) source: Arc<Value>,
    pub(crate) materials: Vec<Value>,
    warnings: Vec<String>,
}

impl PreparedEffect {
    pub fn metadata(&self) -> Value {
        json!({"name":self.name,"seed":self.seed,"origin":self.placement.origin,
            "angles":self.placement.angles,"fingerprint":self.source["dependencyFingerprint"],
            "materialCount":self.source["sharedMaterialInfo"].as_object().map_or(0,|m|m.len()),"maxTimeMs":60000,
            "warnings":self.warnings,
            "limits":["Visual preview only; no gameplay scripts, collisions or sound aliases",
                "Fixed zero wind; unsupported complete effects are rejected",
                "Particle lighting retains the verified editor preview value rather than resampling the scene light grid",
                "FX model surfaces retain the verified original unlit material path"]})
    }
}

fn emitter_axis(angles: [f32; 3]) -> [[f32; 3]; 3] {
    let [pitch, yaw, roll] = angles.map(f32::to_radians);
    let (sp, cp) = pitch.sin_cos();
    let (sy, cy) = yaw.sin_cos();
    let (sr, cr) = roll.sin_cos();
    [
        [cp * cy, cp * sy, -sp],
        [sr * sp * cy - cr * sy, sr * sp * sy + cr * cy, sr * cp],
        [cr * sp * cy + sr * sy, cr * sp * sy - sr * cy, cr * cp],
    ]
}

pub fn prepare_effect(
    game: &Path,
    name: &str,
    placement: EffectPlacement,
    seed: u32,
) -> Result<PreparedEffect, String> {
    prepare_effect_typed(game, name, placement, seed).map_err(|e| e.to_string())
}
fn validate_scene_visual_capability(
    source: &Value,
) -> Result<bool, crate::scene_fx::FxPrepareError> {
    let mut has_lights = false;
    let mut has_draws = false;
    for definition in std::iter::once(source).chain(
        source["dependencies"]
            .as_object()
            .into_iter()
            .flat_map(|r| r.values()),
    ) {
        for element in definition["elements"]
            .as_array()
            .ok_or("Missing effect elements.")?
        {
            if fx::loader::words(element, "editorFlags")
                .iter()
                .any(|flag| flag == "disabled")
            {
                continue;
            }
            has_lights |= element.get("light").is_some() || element.get("spotLight").is_some();
            has_draws |= [
                "billboardSprite",
                "orientedSprite",
                "tail",
                "line",
                "cloud",
                "model",
                "trail",
            ]
            .iter()
            .any(|kind| element.get(*kind).is_some());
        }
    }
    if has_lights && !has_draws {
        return Err(crate::scene_fx::FxPrepareError::capability("scene_lights_only","This effect contains only scene lights; FX lighting of world surfaces is outside this v1."));
    }
    Ok(has_lights)
}
pub fn prepare_effect_typed(
    game: &Path,
    name: &str,
    placement: EffectPlacement,
    seed: u32,
) -> Result<PreparedEffect, crate::scene_fx::FxPrepareError> {
    if seed >= 479 {
        return Err("Effect seed must be between 0 and 478.".into());
    }
    if !placement
        .origin
        .iter()
        .chain(&placement.angles)
        .all(|value| value.is_finite() && value.abs() <= 16_777_216.)
    {
        return Err("Invalid effect placement.".into());
    }
    let mut source = fx::prepare::preflight_typed(game, name)?;
    let has_lights = validate_scene_visual_capability(&source)?;
    let warnings = if has_lights {
        vec!["This effect's emitted lights are not applied to scene surfaces in v1; its supported visual particles are shown.".into()]
    } else {
        Vec::new()
    };
    let name = source["name"]
        .as_str()
        .ok_or("Prepared effect has no name.")?
        .to_string();
    source["sceneDrawSnapshots"] = json!(true);
    source["compactPreviewFrames"] = json!(true);
    source["previewFrameByteBudget"] = json!(fx::prepare::MAX_CLIP_BYTES / 2);
    // Keep the default identity frame absent so existing whole-graph guards do
    // not interpret an unchanged emitter as an unverified custom-frame request.
    if placement.origin != [0.; 3] || placement.angles != [0.; 3] {
        source["previewEmitterFrame"] = json!({"origin":placement.origin,
            "storedAxis":fx::frames::stored_axis(emitter_axis(placement.angles))});
    }
    fx::simulation::validate_cloud_scope(&source)?;
    // Compile the actual placed source before replacing a successful preview.
    fx::simulate(&source, seed, &[])?;
    let materials = fx::prepare::material_manifest(&source)?;
    Ok(PreparedEffect {
        name,
        seed,
        placement,
        source: Arc::new(source),
        materials,
        warnings,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[ignore = "requires WAW_TEST_GAME_PATH; reads installed EFX/material assets without creating a GPU device"]
    fn paused_scene_snapshot_rebuilds_billboards_without_advancing_time() {
        let path = std::env::var_os("WAW_TEST_GAME_PATH").expect("Set WAW_TEST_GAME_PATH");
        let prepared = prepare_effect(
            Path::new(&path),
            "env/fire/fx_static_fire_small_anim",
            EffectPlacement {
                origin: [0.; 3],
                angles: [0.; 3],
            },
            17,
        )
        .unwrap();
        let mut retained = None;
        fx::simulation::stream(&prepared.source, 17, 33, &mut |packet| {
            if packet.frame["time"].as_u64().unwrap() >= 500 {
                retained = packet.redraw;
                return Ok(false);
            }
            Ok(true)
        })
        .unwrap();
        let snapshot = retained.expect("Scene draw snapshot");
        let first = snapshot
            .render([200., 0., 50.], fx::camera::DEFAULT_AXIS)
            .unwrap();
        let rotated = snapshot
            .render(
                [0., -200., 50.],
                [[0., 1., 0.], [-1., 0., 0.], [0., 0., 1.]],
            )
            .unwrap();
        assert_eq!(first["time"], rotated["time"]);
        assert!(!first["draws"].as_array().unwrap().is_empty());
        assert_ne!(
            first["draws"], rotated["draws"],
            "Camera movement must repack the billboard vertices"
        );
        assert_eq!(
            first,
            snapshot
                .render([200., 0., 50.], fx::camera::DEFAULT_AXIS)
                .unwrap(),
            "Redrawing must not consume random state or advance particles"
        );
    }
}

#[cfg(test)]
mod typed_admission_tests {
    use super::*;
    use crate::scene_fx::FxPrepareError;
    #[test]
    fn typed_scene_lights_omitted() {
        assert!(matches!(
            validate_scene_visual_capability(&json!({"elements":[{"light":{}}]})),
            Err(FxPrepareError::Omitted {
                code: "scene_lights_only",
                ..
            })
        ));
    }
    #[test]
    fn typed_visual_retained() {
        assert_eq!(
            validate_scene_visual_capability(
                &json!({"elements":[{"light":{},"billboardSprite":{}}]})
            )
            .unwrap(),
            true
        );
    }
    #[test]
    fn typed_malformed_graph_fatal() {
        assert!(matches!(
            validate_scene_visual_capability(&json!({"elements":null})),
            Err(FxPrepareError::Fatal { .. })
        ));
    }
    #[test]
    fn typed_invalid_placement_fatal_before_io() {
        assert!(matches!(
            prepare_effect_typed(
                Path::new("missing"),
                "x",
                EffectPlacement {
                    origin: [f32::NAN, 0., 0.],
                    angles: [0.; 3]
                },
                17
            ),
            Err(FxPrepareError::Fatal { .. })
        ));
    }
    #[test]
    fn typed_invalid_seed_fatal_before_io() {
        assert!(matches!(
            prepare_effect_typed(
                Path::new("missing"),
                "x",
                EffectPlacement {
                    origin: [0.; 3],
                    angles: [0.; 3]
                },
                479
            ),
            Err(FxPrepareError::Fatal { .. })
        ));
    }
    #[test]
    fn typed_missing_source_fatal() {
        assert!(matches!(
            prepare_effect_typed(
                Path::new("missing"),
                "x",
                EffectPlacement {
                    origin: [0.; 3],
                    angles: [0.; 3]
                },
                17
            ),
            Err(FxPrepareError::Fatal { .. })
        ));
    }
    #[test]
    fn typed_unclassified_unsupported_text_fatal() {
        assert!(matches!(
            FxPrepareError::from("unsupported shader corruption"),
            FxPrepareError::Fatal { .. }
        ));
    }
}
