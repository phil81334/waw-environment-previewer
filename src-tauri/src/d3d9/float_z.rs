//! Original WaW camera-forward R32F scene depth for soft/distortion FX.
//!
//! This is NOT the shadow comparison texture. The pass replays only geometry
//! which owns the already rendered scene's depth, using EQUAL and no depth
//! writes. Alpha-tested coverage comes from that existing depth buffer; changing
//! this to LEQUAL would incorrectly fill transparent holes.
use super::shaders::{RegisterKind, ShaderLibrary, ShaderProgram};
use std::ptr::null_mut;
use windows::Win32::Graphics::Direct3D9::*;

pub type Result<T> = std::result::Result<T, String>;
pub const CLEAR_DISTANCE: f32 = 2_000_000.;
const IDENTITY: [f32; 16] = [
    1., 0., 0., 0., 0., 1., 0., 0., 0., 0., 1., 0., 0., 0., 0., 1.,
];
const DEPTH_FROM_CLIP: [f32; 4] = [0., 0., 0., 1.];

fn error(value: windows::core::Error) -> String {
    value.to_string()
}
fn register(program: &ShaderProgram, name: &str, count: u32) -> Result<u32> {
    program
        .constant(name)
        .filter(|value| value.kind == RegisterKind::Float && value.count >= count)
        .map(|value| value.register)
        .ok_or_else(|| format!("Original float-Z shader lacks {name} ({count} rows)"))
}

/// Matrix registers in row order. This matches the base scene's worldMatrix:
/// local axes/scale followed by origin minus the SAME scene camera position.
pub fn world_rows(
    axis: [[f32; 3]; 3],
    origin: [f32; 3],
    scale: f32,
    eye: [f32; 3],
) -> Result<[f32; 16]> {
    if !axis
        .iter()
        .flatten()
        .chain(origin.iter())
        .chain(eye.iter())
        .all(|v| v.is_finite())
        || !scale.is_finite()
    {
        return Err("Nonfinite float-Z object transform".into());
    }
    let mut world = IDENTITY;
    for row in 0..3 {
        for col in 0..3 {
            world[row * 4 + col] = axis[col][row] * scale;
        }
        world[row * 4 + 3] = origin[row] - eye[row];
    }
    if !world.iter().all(|value| value.is_finite()) {
        return Err("Float-Z object transform overflow".into());
    }
    Ok(world)
}

/// Clip W must be camera-forward distance, as in this renderer's perspective
/// projection (including the original far-cap projection). An orthographic or
/// arbitrary projective matrix needs a different depthFromClip contract.
pub fn validate_camera(rows: &[f32; 16], eye: [f32; 3]) -> Result<()> {
    if !rows.iter().chain(eye.iter()).all(|v| v.is_finite()) {
        return Err("Nonfinite float-Z camera".into());
    }
    let length = (rows[12] * rows[12] + rows[13] * rows[13] + rows[14] * rows[14]).sqrt();
    if (length - 1.).abs() > 0.0001 || rows[15].abs() > 0.0001 {
        return Err("Float-Z requires the base scene's camera-relative perspective matrix; clip W must be forward distance".into());
    }
    Ok(())
}

struct Target {
    texture: IDirect3DTexture9,
    surface: IDirect3DSurface9,
    width: u32,
    height: u32,
}
pub struct FloatZ {
    vertex: IDirect3DVertexShader9,
    pixel: IDirect3DPixelShader9,
    clear_vertex: IDirect3DVertexShader9,
    clear_pixel: IDirect3DPixelShader9,
    clear_declaration: IDirect3DVertexDeclaration9,
    view_projection_register: u32,
    world_register: u32,
    depth_register: u32,
    target: Option<Target>,
}

pub struct FloatZPass<'a> {
    device: &'a IDirect3DDevice9,
    vertex: &'a IDirect3DVertexShader9,
    pixel: &'a IDirect3DPixelShader9,
    camera_rows: &'a [f32; 16],
    eye: [f32; 3],
    view_projection_register: u32,
    world_register: u32,
    depth_register: u32,
}
impl FloatZPass<'_> {
    /// The caller binds exactly the original scene mesh declaration/VB/IB and
    /// material culling, then issues its draw. Do not include sky, helpers without
    /// a depth write, or blended models which did not own the base depth.
    pub fn bind_object(&self, axis: [[f32; 3]; 3], origin: [f32; 3], scale: f32) -> Result<()> {
        let world = world_rows(axis, origin, scale, self.eye)?;
        self.bind_world_rows(&world)
    }
    /// Replay an already verified base-pass world matrix unchanged. FX model
    /// poses can contain transforms that must not be reconstructed through an
    /// axis/scale decomposition: tiny rounding changes can fail depth EQUAL.
    pub fn bind_world_rows(&self, world: &[f32; 16]) -> Result<()> {
        if !world.iter().all(|value| value.is_finite()) {
            return Err("Nonfinite original float-Z world matrix".into());
        }
        unsafe {
            self.device.SetVertexShader(self.vertex).map_err(error)?;
            self.device.SetPixelShader(self.pixel).map_err(error)?;
            self.device
                .SetVertexShaderConstantF(
                    self.view_projection_register,
                    self.camera_rows.as_ptr(),
                    4,
                )
                .map_err(error)?;
            self.device
                .SetVertexShaderConstantF(self.world_register, world.as_ptr(), 4)
                .map_err(error)?;
            self.device
                .SetVertexShaderConstantF(self.depth_register, DEPTH_FROM_CLIP.as_ptr(), 1)
                .map_err(error)?;
            // Reassert coverage after material state changes: cutout coverage is
            // already represented by scene depth, not an opaque replacement PS.
            self.device
                .SetRenderState(D3DRS_ZENABLE, 1)
                .map_err(error)?;
            self.device
                .SetRenderState(D3DRS_ZWRITEENABLE, 0)
                .map_err(error)?;
            self.device
                .SetRenderState(D3DRS_ZFUNC, D3DCMP_EQUAL.0 as u32)
                .map_err(error)?;
            self.device
                .SetRenderState(D3DRS_ALPHATESTENABLE, 0)
                .map_err(error)?;
            self.device
                .SetRenderState(D3DRS_ALPHABLENDENABLE, 0)
                .map_err(error)?;
        }
        Ok(())
    }
}

impl FloatZ {
    /// Programs come from the installed user assets via ShaderLibrary. No shader
    /// recompilation or embedded replacement programs are used by this module.
    pub fn new(device: &IDirect3DDevice9, library: &ShaderLibrary) -> Result<Self> {
        let vertex = library.vertex("floatz_build.hlsl")?;
        let pixel = library.pixel("floatz_build.hlsl")?;
        let clear_vertex = library.vertex("transform_only.hlsl")?;
        let clear_pixel = library.pixel("floatz_clear.hlsl")?;
        let view_projection_register = register(vertex, "viewProjectionMatrix", 4)?;
        let world_register = register(vertex, "worldMatrix", 4)?;
        let depth_register = register(vertex, "depthFromClip", 1)?;
        unsafe {
            let elements = [
                D3DVERTEXELEMENT9 {
                    Stream: 0,
                    Offset: 0,
                    Type: D3DDECLTYPE_FLOAT3.0 as u8,
                    Method: 0,
                    Usage: D3DDECLUSAGE_POSITION.0 as u8,
                    UsageIndex: 0,
                },
                D3DVERTEXELEMENT9 {
                    Stream: 255,
                    Offset: 0,
                    Type: D3DDECLTYPE_UNUSED.0 as u8,
                    Method: 0,
                    Usage: 0,
                    UsageIndex: 0,
                },
            ];
            Ok(Self {
                vertex: device
                    .CreateVertexShader(vertex.words.as_ptr())
                    .map_err(error)?,
                pixel: device
                    .CreatePixelShader(pixel.words.as_ptr())
                    .map_err(error)?,
                clear_vertex: device
                    .CreateVertexShader(clear_vertex.words.as_ptr())
                    .map_err(error)?,
                clear_pixel: device
                    .CreatePixelShader(clear_pixel.words.as_ptr())
                    .map_err(error)?,
                clear_declaration: device
                    .CreateVertexDeclaration(elements.as_ptr())
                    .map_err(error)?,
                view_projection_register,
                world_register,
                depth_register,
                target: None,
            })
        }
    }
    pub fn resize(&mut self, device: &IDirect3DDevice9, width: u32, height: u32) -> Result<()> {
        if width == 0 || height == 0 || width > 8192 || height > 8192 {
            return Err("Invalid float-Z dimensions".into());
        }
        if self
            .target
            .as_ref()
            .is_some_and(|value| value.width == width && value.height == height)
        {
            return Ok(());
        }
        unsafe {
            let mut texture = None;
            device
                .CreateTexture(
                    width,
                    height,
                    1,
                    D3DUSAGE_RENDERTARGET as u32,
                    D3DFMT_R32F,
                    D3DPOOL_DEFAULT,
                    &mut texture,
                    null_mut(),
                )
                .map_err(|e| format!("Scene float-Z R32F target unavailable: {e}"))?;
            let texture = texture.ok_or("No scene float-Z texture")?;
            let surface = texture.GetSurfaceLevel(0).map_err(error)?;
            // Retain the previous target if creation of its replacement fails.
            self.target = Some(Target {
                texture,
                surface,
                width,
                height,
            });
        }
        Ok(())
    }
    pub fn texture(&self) -> Result<&IDirect3DTexture9> {
        self.target
            .as_ref()
            .map(|value| &value.texture)
            .ok_or_else(|| "Scene float-Z has not been sized".into())
    }
    pub fn size(&self) -> Option<(u32, u32)> {
        self.target
            .as_ref()
            .map(|value| (value.width, value.height))
    }
    pub fn diagnostics(&self) -> serde_json::Value {
        serde_json::json!({"initialized":self.target.is_some(),"size":self.size(),"format":"R32F",
            "clearDistance":CLEAR_DISTANCE,"depthComparison":"EQUAL","depthWrite":false,
            "depthMeaning":"camera-forward linear distance","coverage":"existing scene depth owners",
            "verification":"GPU readback is explicit test evidence; no bitwise parity claim"})
    }
    /// Call outside an existing BeginScene. The provided scene depth MUST belong
    /// to this camera and exact dimensions, after all participating opaque/cutout
    /// geometry. The callback must replay that same geometry, including supported
    /// FX models if their soft-particle intersections are claimed.
    pub fn render<F>(
        &self,
        device: &IDirect3DDevice9,
        scene_depth: &IDirect3DSurface9,
        camera_rows: &[f32; 16],
        eye: [f32; 3],
        mut draw: F,
    ) -> Result<()>
    where
        F: FnMut(&IDirect3DDevice9, &FloatZPass<'_>) -> Result<()>,
    {
        validate_camera(camera_rows, eye)?;
        let target = self
            .target
            .as_ref()
            .ok_or("Scene float-Z has not been sized")?;
        unsafe {
            let mut description = D3DSURFACE_DESC::default();
            scene_depth.GetDesc(&mut description).map_err(error)?;
            if description.Width != target.width
                || description.Height != target.height
                || description.MultiSampleType != D3DMULTISAMPLE_NONE
            {
                return Err("Float-Z must match the base scene's non-MSAA depth dimensions".into());
            }
            let saved = device.CreateStateBlock(D3DSBT_ALL).map_err(error)?;
            let old_color = device.GetRenderTarget(0).map_err(error)?;
            let old_depth = device.GetDepthStencilSurface().ok();
            let mut viewport = D3DVIEWPORT9::default();
            device.GetViewport(&mut viewport).map_err(error)?;
            let operation = (|| -> Result<()> {
                // Avoid simultaneous sampling/rendering if the previous FX pass
                // left this texture bound at its feather or distortion sampler.
                for slot in 0..16 {
                    device
                        .SetTexture(slot, None::<&IDirect3DBaseTexture9>)
                        .map_err(error)?;
                }
                device
                    .SetDepthStencilSurface(None::<&IDirect3DSurface9>)
                    .map_err(error)?;
                device.SetRenderTarget(0, &target.surface).map_err(error)?;
                device
                    .SetViewport(&D3DVIEWPORT9 {
                        X: 0,
                        Y: 0,
                        Width: target.width,
                        Height: target.height,
                        MinZ: 0.,
                        MaxZ: 1.,
                    })
                    .map_err(error)?;
                for (state, value) in [
                    (D3DRS_ZENABLE, 0),
                    (D3DRS_ZWRITEENABLE, 0),
                    (D3DRS_ALPHABLENDENABLE, 0),
                    (D3DRS_ALPHATESTENABLE, 0),
                    (D3DRS_STENCILENABLE, 0),
                    (D3DRS_SCISSORTESTENABLE, 0),
                    (D3DRS_CLIPPLANEENABLE, 0),
                    (D3DRS_CULLMODE, D3DCULL_NONE.0 as u32),
                    (D3DRS_FILLMODE, D3DFILL_SOLID.0 as u32),
                    (D3DRS_COLORWRITEENABLE, 15),
                    (D3DRS_SRGBWRITEENABLE, 0),
                    (D3DRS_DEPTHBIAS, 0),
                    (D3DRS_SLOPESCALEDEPTHBIAS, 0),
                ] {
                    device.SetRenderState(state, value).map_err(error)?;
                }
                device.BeginScene().map_err(error)?;
                let inside = (|| -> Result<()> {
                    // D3DCOLOR Clear would clamp this to 0..1. The original
                    // floatz_clear pixel program really writes 2,000,000 in R32F.
                    device
                        .SetVertexDeclaration(&self.clear_declaration)
                        .map_err(error)?;
                    device.SetVertexShader(&self.clear_vertex).map_err(error)?;
                    device.SetPixelShader(&self.clear_pixel).map_err(error)?;
                    device
                        .SetVertexShaderConstantF(0, IDENTITY.as_ptr(), 4)
                        .map_err(error)?;
                    device
                        .SetVertexShaderConstantF(4, IDENTITY.as_ptr(), 4)
                        .map_err(error)?;
                    let triangle: [f32; 9] = [-1., -1., 0., -1., 3., 0., 3., -1., 0.];
                    device
                        .DrawPrimitiveUP(D3DPT_TRIANGLELIST, 1, triangle.as_ptr().cast(), 12)
                        .map_err(error)?;
                    device.SetDepthStencilSurface(scene_depth).map_err(error)?;
                    device.SetRenderState(D3DRS_ZENABLE, 1).map_err(error)?;
                    device
                        .SetRenderState(D3DRS_ZWRITEENABLE, 0)
                        .map_err(error)?;
                    device
                        .SetRenderState(D3DRS_ZFUNC, D3DCMP_EQUAL.0 as u32)
                        .map_err(error)?;
                    let pass = FloatZPass {
                        device,
                        vertex: &self.vertex,
                        pixel: &self.pixel,
                        camera_rows,
                        eye,
                        view_projection_register: self.view_projection_register,
                        world_register: self.world_register,
                        depth_register: self.depth_register,
                    };
                    draw(device, &pass)
                })();
                let end = device.EndScene().map_err(error);
                inside.and(end)
            })();
            // Render targets/depth attachments are not captured by D3DSBT_ALL.
            // Attempt each restoration even when an earlier one fails.
            let mut restore_error = None;
            for result in [
                device.SetDepthStencilSurface(None::<&IDirect3DSurface9>),
                device.SetRenderTarget(0, &old_color),
                device.SetDepthStencilSurface(old_depth.as_ref()),
                saved.Apply(),
                device.SetViewport(&viewport),
            ] {
                if let Err(value) = result {
                    restore_error.get_or_insert_with(|| value.to_string());
                }
            }
            match (operation, restore_error) {
                (Err(a), Some(b)) => Err(format!("{a}; restoring float-Z scene state failed: {b}")),
                (Err(a), None) => Err(a),
                (Ok(()), Some(b)) => Err(b),
                (Ok(()), None) => Ok(()),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn apply(rows: &[f32; 16], v: [f32; 4]) -> [f32; 4] {
        std::array::from_fn(|row| (0..4).map(|col| rows[row * 4 + col] * v[col]).sum())
    }
    #[test]
    fn camera_forward_depth_is_not_distance_or_normalized_depth() {
        // Forward X camera, 90-degree vertical FoV, original far-cap near4.
        let a = 2047. / 2048.;
        let projection = [
            0.,
            -0.75 * a,
            0.,
            0.,
            0.,
            0.,
            a,
            0.,
            a,
            0.,
            0.,
            -4. * a,
            1.,
            0.,
            0.,
            0.,
        ];
        validate_camera(&projection, [10., 20., 30.]).unwrap();
        let world = world_rows(
            [[1., 0., 0.], [0., 1., 0.], [0., 0., 1.]],
            [110., 70., 30.],
            1.,
            [10., 20., 30.],
        )
        .unwrap();
        let clip = apply(&projection, apply(&world, [0., 0., 0., 1.]));
        assert_eq!(clip[3], 100.);
        assert_ne!(clip[2] / clip[3], 100.);
        assert_ne!(50.0_f32.hypot(100.), 100.);
    }
    #[test]
    fn object_axis_scale_and_camera_translation_match_scene_registers() {
        let axis = [[0., 1., 0.], [-1., 0., 0.], [0., 0., 1.]];
        let world = world_rows(axis, [15., 25., 35.], 2., [10., 20., 30.]).unwrap();
        assert_eq!(apply(&world, [1., 2., 3., 1.]), [1., 7., 11., 1.]);
    }
    #[test]
    fn rejects_invalid_or_orthographic_camera() {
        assert!(validate_camera(&IDENTITY, [0.; 3]).is_err());
        let mut bad = IDENTITY;
        bad[0] = f32::NAN;
        assert!(validate_camera(&bad, [0.; 3]).is_err());
        assert!(world_rows(
            [[1., 0., 0.], [0., 1., 0.], [0., 0., 1.]],
            [f32::INFINITY, 0., 0.],
            1.,
            [0.; 3]
        )
        .is_err());
    }
}
