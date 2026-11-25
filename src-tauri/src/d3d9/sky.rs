//! Named-sky program selection and explicit preview constants.
//!
//! The sky is camera-centred geometry, rather than an infinitely distant colour
//! clear. These helpers retain the three layer types already supported by the
//! frozen WebGL renderer. They own no device or wall clock: captures and still
//! comparisons must be able to select exactly the same preview instant.

use serde_json::Value;

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct PreviewClock {
    pub seconds: f32,
    pub transition: f32,
}

fn finite_number(value: &Value, fallback: f32, label: &str) -> Result<f32, String> {
    if value.is_null() {
        return Ok(fallback);
    }
    let number = value
        .as_f64()
        .filter(|number| number.is_finite() && number.abs() <= f32::MAX as f64)
        .ok_or_else(|| format!("Invalid {label}."))?;
    Ok(number as f32)
}

impl PreviewClock {
    pub const ZERO: Self = Self {
        seconds: 0.0,
        transition: 0.0,
    };

    pub fn from_view(view: &Value) -> Result<Self, String> {
        let settings = &view["worldspawn"];
        Ok(Self {
            seconds: finite_number(&settings["time"], 0.0, "sky preview time")?,
            // WebGL clamps this at the shader uniform, rather than altering the
            // stored UI setting. Do the same at this rendering boundary.
            transition: finite_number(&settings["transition"], 0.0, "sky transition")?
                .clamp(0.0, 1.0),
        })
    }

    /// Original sky VS uses gameTime.w; the other components remain unused.
    pub fn game_time(self) -> [f32; 4] {
        [0.0, 0.0, 0.0, self.seconds]
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct SkyMaterial {
    pub vertex_program: &'static str,
    pub pixel_program: &'static str,
    pub scroll: [f32; 2],
    pub sort: u8,
    pub is_transition: bool,
    pub fog_supported: bool,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct SkyConstants {
    pub game_time: [f32; 4],
    pub uv_scroll: [f32; 4],
    pub sky_transition: [f32; 4],
    pub fog_consts: [f32; 4],
    pub fog_color: [f32; 4],
}

impl SkyMaterial {
    pub fn from_layer(layer: &Value) -> Result<Self, String> {
        let family = layer["techset"]
            .as_str()
            .ok_or("Sky layer has no techset.")?;
        let (vertex_program, pixel_program) = match family {
            "sky_noncubemap" => ("sky_noncubemap_dtex.hlsl", "sky_noncubemap.hlsl"),
            "sky_noncubemap_add" => ("sky_noncubemap_dtex_add.hlsl", "sky_noncubemap_add.hlsl"),
            "sky_noncubemap_transition" => (
                "sky_noncubemap_transition_dtex.hlsl",
                "sky_noncubemap_transition.hlsl",
            ),
            _ => return Err(format!("Unsupported named-sky techset: {family}.")),
        };
        let scroll = layer["scroll"]
            .as_array()
            .ok_or("Sky layer has no UV scroll.")?;
        if scroll.len() != 2 {
            return Err("Sky UV scroll must contain two components.".into());
        }
        let textures = layer["textures"]
            .as_array()
            .ok_or("Sky layer has no texture bindings.")?;
        let has_binding = |semantic: &str| {
            textures
                .iter()
                .any(|binding| binding["semantic"] == semantic)
        };
        if !has_binding("colorMap") {
            return Err("Sky layer has no colour map.".into());
        }
        let is_transition = family == "sky_noncubemap_transition";
        if is_transition && !has_binding("colorMap01") {
            return Err("Transition sky is missing its second image.".into());
        }
        let sort = layer["sort"]
            .as_u64()
            .and_then(|value| u8::try_from(value).ok())
            .ok_or("Invalid sky layer sort key.")?;
        Ok(Self {
            vertex_program,
            pixel_program,
            scroll: [
                finite_number(&scroll[0], 0.0, "sky U scroll")?,
                finite_number(&scroll[1], 0.0, "sky V scroll")?,
            ],
            sort,
            is_transition,
            // The existing Fog playground proves normal Pel1 sky fog only.
            // Preserve that scope for additive and transition layers.
            fog_supported: family == "sky_noncubemap",
        })
    }

    pub fn constants(
        self,
        clock: PreviewClock,
        fog_enabled: bool,
        fog_consts: [f32; 4],
        fog_color: [f32; 4],
    ) -> SkyConstants {
        let apply_fog = fog_enabled && self.fog_supported;
        SkyConstants {
            game_time: clock.game_time(),
            uv_scroll: [self.scroll[0], self.scroll[1], 0.0, 0.0],
            sky_transition: [clock.transition, 0.0, 0.0, 0.0],
            fog_consts: if apply_fog { fog_consts } else { [0.0; 4] },
            fog_color: if apply_fog { fog_color } else { [0.0; 4] },
        }
    }
}
