//! Primary local-light hardware depth shadows for the isolated native renderer.
//!
//! The selected primary omni or spot light casts one planar shadow.
//! The original receiver's four hardware-PCF reads require comparison-capable D24S8.
use super::shaders::{RegisterKind, ShaderLibrary, ShaderProgram};
use crate::Light;
use std::ptr::null_mut;
use windows::Win32::Graphics::Direct3D9::*;

pub type Result<T> = std::result::Result<T, String>;
pub const ATLAS_WIDTH: u32 = 512;
pub const ATLAS_HEIGHT: u32 = 2048;
pub const TILE_SIZE: u32 = 512;
pub const DEPTH_BIAS: f32 = 2.5 / 65536.0;
pub const SLOPE_BIAS: f32 = 2.0;
pub const PIXEL_ADJUST: [f32; 4] = [1.0 / 2048.0, 1.0 / 4096.0, 1.0 / 1024.0, -1.0 / 8192.0];
const HOMOGENEOUS_BIAS: f32 = 0.9994999766349792;
const IDENTITY: [f32; 16] = [
    1., 0., 0., 0., 0., 1., 0., 0., 0., 0., 1., 0., 0., 0., 0., 1.,
];

fn error(value: windows::core::Error) -> String {
    value.to_string()
}
fn dot(a: [f32; 3], b: [f32; 3]) -> f32 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}
fn cross(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
    [
        a[1] * b[2] - a[2] * b[1],
        a[2] * b[0] - a[0] * b[2],
        a[0] * b[1] - a[1] * b[0],
    ]
}
fn normalize(v: [f32; 3]) -> Result<[f32; 3]> {
    let length = dot(v, v).sqrt();
    if !length.is_finite() || length < 1e-12 {
        return Err("Invalid local shadow direction".into());
    }
    Ok(v.map(|x| x / length))
}
fn multiply(a: &[f32; 16], b: &[f32; 16]) -> [f32; 16] {
    let mut out = [0.; 16];
    for col in 0..4 {
        for row in 0..4 {
            out[col * 4 + row] = (0..4).map(|k| a[k * 4 + row] * b[col * 4 + k]).sum();
        }
    }
    out
}
fn rows(matrix: &[f32; 16]) -> [f32; 16] {
    let mut out = [0.; 16];
    for row in 0..4 {
        for col in 0..4 {
            out[row * 4 + col] = matrix[col * 4 + row];
        }
    }
    out
}

pub fn eligible(light: &Light) -> bool {
    matches!(light.r#type, 2 | 3) && light.shadow_eligible && light.cos_outer > 0.0
}

/// Column-major matrices, matching renderer/math.js. D3D constant rows are
/// obtained with the methods below; never upload these arrays unchanged as rows.
#[derive(Clone, Copy, Debug)]
pub struct ShadowMatrices {
    pub view_projection: [f32; 16],
    pub lookup: [f32; 16],
}
impl ShadowMatrices {
    pub fn for_light(light: &Light) -> Result<Self> {
        Self::new(light.origin, light.direction, light.cos_outer, light.radius)
    }
    pub fn new(origin: [f32; 3], direction: [f32; 3], cos_outer: f32, radius: f32) -> Result<Self> {
        if !origin.iter().all(|x| x.is_finite()) || !cos_outer.is_finite() || !radius.is_finite() {
            return Err("Nonfinite local shadow projection".into());
        }
        let forward = normalize(direction)?.map(|v| -v);
        let mut axis = 0;
        for i in 1..3 {
            if forward[i] * forward[i] < forward[axis] * forward[axis] {
                axis = i;
            }
        }
        let mut up = forward.map(|v| -v * forward[axis]);
        up[axis] += 1.;
        let up = normalize(up)?;
        let right = cross(forward, up);
        let view = [
            right[0],
            up[0],
            forward[0],
            0.,
            right[1],
            up[1],
            forward[1],
            0.,
            right[2],
            up[2],
            forward[2],
            0.,
            -dot(origin, right),
            -dot(origin, up),
            -dot(origin, forward),
            1.,
        ];
        let cosine = cos_outer.clamp(0.0001, 0.999999);
        let focal = 1. / cosine.acos().tan();
        let far = radius.max(2.);
        let projection = [
            focal,
            0.,
            0.,
            0.,
            0.,
            focal,
            0.,
            0.,
            0.,
            0.,
            far / (far - 1.),
            1.,
            0.,
            0.,
            -far / (far - 1.),
            0.,
        ];
        let view_projection = multiply(&projection, &view);
        let atlas = [
            0.5, 0., 0., 0., 0., -0.125, 0., 0., 0., 0., 1., 0., 0.5, 0.125, 0., 1.,
        ];
        let mut lookup = multiply(&atlas, &view_projection);
        // Original host biases ONE homogeneous coefficient, not the Z row.
        lookup[11] *= HOMOGENEOUS_BIAS;
        if !view_projection
            .iter()
            .chain(lookup.iter())
            .all(|v| v.is_finite())
        {
            return Err("Local shadow projection overflow".into());
        }
        Ok(Self {
            view_projection,
            lookup,
        })
    }
    /// Receiver worldMatrix is camera-relative, so rebase the world-space lookup
    /// by that SAME camera origin before uploading shadowLookupMatrix.
    pub fn lookup_rows(&self, eye: [f32; 3]) -> [f32; 16] {
        let mut translation = IDENTITY;
        translation[12..15].copy_from_slice(&eye);
        rows(&multiply(&self.lookup, &translation))
    }
    /// Transform local mesh coordinates to world space before the light camera.
    /// Axis vectors use the same axis[col][row] convention as renderer::Draw.
    pub fn caster_rows(&self, axis: [[f32; 3]; 3], origin: [f32; 3], scale: f32) -> [f32; 16] {
        let mut world = IDENTITY;
        for col in 0..3 {
            for row in 0..3 {
                world[col * 4 + row] = axis[col][row] * scale;
            }
        }
        world[12..15].copy_from_slice(&origin);
        rows(&multiply(&self.view_projection, &world))
    }
}

/// Borrowed pass information; the parent supplies its own mesh buffers/draws.
pub struct ShadowPass<'a> {
    pub matrices: &'a ShadowMatrices,
    vertex: &'a IDirect3DVertexShader9,
    pixel: &'a IDirect3DPixelShader9,
    vertex_metadata: &'a ShaderProgram,
    alpha: Option<&'a AlphaProgram>,
}
struct AlphaProgram {
    vertex: IDirect3DVertexShader9,
    pixel: IDirect3DPixelShader9,
    vertex_metadata: ShaderProgram,
    pixel_metadata: ShaderProgram,
}
impl ShadowPass<'_> {
    pub fn has_alpha_caster(&self) -> bool {
        self.alpha.is_some()
    }
    /// Bind the original opaque transform-only caster for one mesh. The caller
    /// still binds its normal vertex declaration, vertex/index buffers and draw.
    /// This deliberately matches WebGL's opaque depth pass; alpha-test foliage
    /// caster variants need their own material-aware path rather than false parity.
    pub fn bind_object(
        &self,
        device: &IDirect3DDevice9,
        axis: [[f32; 3]; 3],
        origin: [f32; 3],
        scale: f32,
    ) -> Result<()> {
        let matrix = self.matrices.caster_rows(axis, origin, scale);
        unsafe {
            device.SetVertexShader(self.vertex).map_err(error)?;
            device.SetPixelShader(self.pixel).map_err(error)?;
            device
                .SetRenderState(D3DRS_ALPHATESTENABLE, 1)
                .map_err(error)?;
            device
                .SetRenderState(D3DRS_ALPHAFUNC, D3DCMP_GREATEREQUAL.0 as u32)
                .map_err(error)?;
            device.SetRenderState(D3DRS_ALPHAREF, 128).map_err(error)?;
            let register = self
                .vertex_metadata
                .constant("worldViewProjectionMatrix")
                .filter(|v| v.kind == RegisterKind::Float && v.count >= 4)
                .map(|v| v.register)
                .unwrap_or(0);
            device
                .SetVertexShaderConstantF(register, matrix.as_ptr(), 4)
                .map_err(error)?;
            if let Some(value) = self.vertex_metadata.constant("worldMatrix") {
                if value.kind == RegisterKind::Float && value.count >= 4 {
                    device
                        .SetVertexShaderConstantF(value.register, IDENTITY.as_ptr(), 4)
                        .map_err(error)?;
                }
            }
        }
        Ok(())
    }
    /// Cutout-model variant. Alpha modes match the existing material reader:
    /// 0=disabled, 1=>0, 2=<128/255, 3=>=128/255. Blended meshes should be skipped
    /// by the caller, as in the WebGL model-depth pass. Texture/sampler bindings
    /// already established by a material-aware caller are preserved except the
    /// color image itself; this method supplies the original alpha caster sampler.
    pub fn bind_alpha_object(
        &self,
        device: &IDirect3DDevice9,
        axis: [[f32; 3]; 3],
        origin: [f32; 3],
        scale: f32,
        color: &IDirect3DTexture9,
        alpha_mode: u8,
    ) -> Result<()> {
        if alpha_mode == 0 {
            return self.bind_object(device, axis, origin, scale);
        }
        let program = self
            .alpha
            .ok_or("Original alpha-tested shadow caster is unavailable; cutout shadow skipped")?;
        let matrix = self.matrices.caster_rows(axis, origin, scale);
        unsafe {
            device.SetVertexShader(&program.vertex).map_err(error)?;
            device.SetPixelShader(&program.pixel).map_err(error)?;
            let register = program
                .vertex_metadata
                .constant("worldViewProjectionMatrix")
                .filter(|v| v.kind == RegisterKind::Float && v.count >= 4)
                .map(|v| v.register)
                .unwrap_or(0);
            device
                .SetVertexShaderConstantF(register, matrix.as_ptr(), 4)
                .map_err(error)?;
            if let Some(value) = program.vertex_metadata.constant("worldMatrix") {
                if value.kind == RegisterKind::Float && value.count >= 4 {
                    device
                        .SetVertexShaderConstantF(value.register, IDENTITY.as_ptr(), 4)
                        .map_err(error)?;
                }
            }
            let sampler = program
                .pixel_metadata
                .constant("colorMapSampler")
                .filter(|v| v.kind == RegisterKind::Sampler)
                .map(|v| v.register)
                .unwrap_or(0);
            device.SetTexture(sampler, color).map_err(error)?;
            for (state, value) in [
                (D3DSAMP_ADDRESSU, D3DTADDRESS_WRAP.0 as u32),
                (D3DSAMP_ADDRESSV, D3DTADDRESS_WRAP.0 as u32),
                (D3DSAMP_MINFILTER, D3DTEXF_LINEAR.0 as u32),
                (D3DSAMP_MAGFILTER, D3DTEXF_LINEAR.0 as u32),
                (D3DSAMP_MIPFILTER, D3DTEXF_LINEAR.0 as u32),
                (D3DSAMP_SRGBTEXTURE, 0),
            ] {
                device
                    .SetSamplerState(sampler, state, value)
                    .map_err(error)?;
            }
            let (function, reference) = match alpha_mode {
                1 => (D3DCMP_GREATER, 0),
                2 => (D3DCMP_LESS, 128),
                3 => (D3DCMP_GREATEREQUAL, 128),
                _ => return Err("Invalid shadow alpha test mode".into()),
            };
            device
                .SetRenderState(D3DRS_ALPHATESTENABLE, 1)
                .map_err(error)?;
            device
                .SetRenderState(D3DRS_ALPHAFUNC, function.0 as u32)
                .map_err(error)?;
            device
                .SetRenderState(D3DRS_ALPHAREF, reference)
                .map_err(error)?;
        }
        Ok(())
    }
}

pub struct DynamicShadow {
    texture: IDirect3DTexture9,
    depth: IDirect3DSurface9,
    color: IDirect3DSurface9,
    vertex: IDirect3DVertexShader9,
    pixel: IDirect3DPixelShader9,
    vertex_metadata: ShaderProgram,
    alpha: Option<AlphaProgram>,
    pub alpha_warning: Option<String>,
    pub color_target_format: &'static str,
}
impl DynamicShadow {
    /// Creation failure must become an explicit shadow warning, never a switch
    /// to an unverified raw-depth format or a different GPU.
    pub fn new(device: &IDirect3DDevice9, library: &ShaderLibrary) -> Result<Self> {
        let vertex_metadata = library.vertex("transform_only.hlsl")?.clone();
        let pixel_metadata = library.pixel("null.hlsl")?;
        unsafe {
            let mut caps = D3DCAPS9::default();
            device.GetDeviceCaps(&mut caps).map_err(error)?;
            let required_bias =
                (D3DPRASTERCAPS_DEPTHBIAS | D3DPRASTERCAPS_SLOPESCALEDEPTHBIAS) as u32;
            if caps.RasterCaps & required_bias != required_bias
                || caps.MaxTextureWidth < ATLAS_WIDTH
                || caps.MaxTextureHeight < ATLAS_HEIGHT
            {
                return Err(
                    "Device lacks required local-shadow atlas or depth-bias capabilities".into(),
                );
            }
            let mut texture = None;
            device
                .CreateTexture(
                    ATLAS_WIDTH,
                    ATLAS_HEIGHT,
                    1,
                    D3DUSAGE_DEPTHSTENCIL as u32,
                    D3DFMT_D24S8,
                    D3DPOOL_DEFAULT,
                    &mut texture,
                    null_mut(),
                )
                .map_err(|e| format!("Preferred D24S8 shadow texture unavailable: {e}"))?;
            let texture = texture.ok_or("No D24S8 shadow texture")?;
            let depth = texture.GetSurfaceLevel(0).map_err(error)?;
            let mut color = None;
            let null_format = D3DFORMAT(u32::from_le_bytes(*b"NULL"));
            let color_target_format = if device
                .CreateRenderTarget(
                    ATLAS_WIDTH,
                    ATLAS_HEIGHT,
                    null_format,
                    D3DMULTISAMPLE_NONE,
                    0,
                    false,
                    &mut color,
                    null_mut(),
                )
                .is_ok()
            {
                "NULL"
            } else {
                // A real color RT is harmless with all color writes disabled;
                // the sampled resource remains the original D24S8 texture.
                device
                    .CreateRenderTarget(
                        ATLAS_WIDTH,
                        ATLAS_HEIGHT,
                        D3DFMT_A8R8G8B8,
                        D3DMULTISAMPLE_NONE,
                        0,
                        false,
                        &mut color,
                        null_mut(),
                    )
                    .map_err(error)?;
                "A8R8G8B8"
            };
            let alpha = (|| -> Result<AlphaProgram> {
                let vertex_metadata = library.vertex("vertalpha_simple_atest.hlsl")?.clone();
                let pixel_metadata = library.pixel("vertalpha_simple_atest.hlsl")?.clone();
                Ok(AlphaProgram {
                    vertex: device
                        .CreateVertexShader(vertex_metadata.words.as_ptr())
                        .map_err(error)?,
                    pixel: device
                        .CreatePixelShader(pixel_metadata.words.as_ptr())
                        .map_err(error)?,
                    vertex_metadata,
                    pixel_metadata,
                })
            })();
            let alpha_warning = alpha.as_ref().err().cloned();
            Ok(Self {
                texture,
                depth,
                color: color.ok_or("No shadow color attachment")?,
                vertex: device
                    .CreateVertexShader(vertex_metadata.words.as_ptr())
                    .map_err(error)?,
                pixel: device
                    .CreatePixelShader(pixel_metadata.words.as_ptr())
                    .map_err(error)?,
                vertex_metadata,
                alpha: alpha.ok(),
                alpha_warning,
                color_target_format,
            })
        }
    }
    /// Call OUTSIDE another BeginScene/EndScene. All device state and the caller's
    /// targets/viewport are restored even if the mesh callback returns an error.
    /// Clear the entire atlas every update, including the three unused tiles.
    pub fn render<F>(
        &self,
        device: &IDirect3DDevice9,
        light: Option<&Light>,
        mut draw: F,
    ) -> Result<Option<ShadowMatrices>>
    where
        F: FnMut(&IDirect3DDevice9, &ShadowPass<'_>) -> Result<()>,
    {
        let matrices = light
            .filter(|value| eligible(value))
            .map(ShadowMatrices::for_light)
            .transpose()?;
        unsafe {
            let saved = device.CreateStateBlock(D3DSBT_ALL).map_err(error)?;
            let old_color = device.GetRenderTarget(0).map_err(error)?;
            let old_depth = device.GetDepthStencilSurface().ok();
            let mut old_viewport = D3DVIEWPORT9::default();
            device.GetViewport(&mut old_viewport).map_err(error)?;
            let operation = (|| -> Result<()> {
                for unit in 0..16 {
                    device
                        .SetTexture(unit, None::<&IDirect3DBaseTexture9>)
                        .map_err(error)?;
                }
                device
                    .SetDepthStencilSurface(None::<&IDirect3DSurface9>)
                    .map_err(error)?;
                device.SetRenderTarget(0, &self.color).map_err(error)?;
                device.SetDepthStencilSurface(&self.depth).map_err(error)?;
                device
                    .SetViewport(&D3DVIEWPORT9 {
                        X: 0,
                        Y: 0,
                        Width: ATLAS_WIDTH,
                        Height: ATLAS_HEIGHT,
                        MinZ: 0.,
                        MaxZ: 1.,
                    })
                    .map_err(error)?;
                for (state, value) in [
                    (D3DRS_ZENABLE, 1),
                    (D3DRS_ZWRITEENABLE, 1),
                    (D3DRS_ZFUNC, D3DCMP_LESSEQUAL.0 as u32),
                    (D3DRS_CULLMODE, D3DCULL_NONE.0 as u32),
                    (D3DRS_COLORWRITEENABLE, 0),
                    (D3DRS_ALPHABLENDENABLE, 0),
                    (D3DRS_ALPHATESTENABLE, 1),
                    (D3DRS_ALPHAFUNC, D3DCMP_GREATEREQUAL.0 as u32),
                    (D3DRS_ALPHAREF, 128),
                    (D3DRS_SCISSORTESTENABLE, 0),
                    (D3DRS_STENCILENABLE, 0),
                    (D3DRS_CLIPPLANEENABLE, 0),
                    (D3DRS_FILLMODE, D3DFILL_SOLID.0 as u32),
                    (D3DRS_DEPTHBIAS, DEPTH_BIAS.to_bits()),
                    (D3DRS_SLOPESCALEDEPTHBIAS, SLOPE_BIAS.to_bits()),
                ] {
                    device.SetRenderState(state, value).map_err(error)?;
                }
                device
                    .Clear(0, std::ptr::null(), D3DCLEAR_ZBUFFER as u32, 0, 1., 0)
                    .map_err(error)?;
                if let Some(ref matrices) = matrices {
                    device
                        .SetViewport(&D3DVIEWPORT9 {
                            X: 0,
                            Y: 0,
                            Width: TILE_SIZE,
                            Height: TILE_SIZE,
                            MinZ: 0.,
                            MaxZ: 1.,
                        })
                        .map_err(error)?;
                    let pass = ShadowPass {
                        matrices,
                        vertex: &self.vertex,
                        pixel: &self.pixel,
                        vertex_metadata: &self.vertex_metadata,
                        alpha: self.alpha.as_ref(),
                    };
                    device.BeginScene().map_err(error)?;
                    let result = draw(device, &pass);
                    let end = device.EndScene().map_err(error);
                    result.and(end)?;
                }
                Ok(())
            })();
            let restore = (|| -> Result<()> {
                // Surfaces are not part of D3DSBT_ALL. Detach depth while changing
                // color targets so the old scene size cannot conflict with atlas.
                device
                    .SetDepthStencilSurface(None::<&IDirect3DSurface9>)
                    .map_err(error)?;
                device.SetRenderTarget(0, &old_color).map_err(error)?;
                device
                    .SetDepthStencilSurface(old_depth.as_ref())
                    .map_err(error)?;
                saved.Apply().map_err(error)?;
                device.SetViewport(&old_viewport).map_err(error)?;
                Ok(())
            })();
            match (operation, restore) {
                (Err(a), Err(b)) => Err(format!("{a}; restoring shadow pass failed: {b}")),
                (Err(a), _) => Err(a),
                (_, Err(b)) => Err(b),
                _ => Ok(matrices),
            }
        }
    }
    /// Bind only to an original *hardware* shadow receiver shader. The fallback
    /// software-comparison shader would compare the hardware result a second time.
    pub fn bind(&self, device: &IDirect3DDevice9, sampler: u32) -> Result<()> {
        unsafe {
            device.SetTexture(sampler, &self.texture).map_err(error)?;
            for (state, value) in [
                (D3DSAMP_ADDRESSU, D3DTADDRESS_CLAMP.0 as u32),
                (D3DSAMP_ADDRESSV, D3DTADDRESS_CLAMP.0 as u32),
                (D3DSAMP_MINFILTER, D3DTEXF_LINEAR.0 as u32),
                (D3DSAMP_MAGFILTER, D3DTEXF_LINEAR.0 as u32),
                (D3DSAMP_MIPFILTER, D3DTEXF_NONE.0 as u32),
                (D3DSAMP_SRGBTEXTURE, 0),
            ] {
                device
                    .SetSamplerState(sampler, state, value)
                    .map_err(error)?;
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn transform(matrix: &[f32; 16], point: [f32; 4]) -> [f32; 4] {
        std::array::from_fn(|row| (0..4).map(|col| matrix[col * 4 + row] * point[col]).sum())
    }
    fn close(a: f32, b: f32) {
        assert!((a - b).abs() < 0.0002, "{a} != {b}");
    }
    #[test]
    fn light_axis_is_centered_and_near_far_are_d3d_depth() {
        let value = ShadowMatrices::new([2., 3., 4.], [0., 0., 1.], 0.5, 200.).unwrap();
        for (distance, z) in [(1., 0.), (200., 1.)] {
            let p = transform(&value.view_projection, [2., 3., 4. - distance, 1.]);
            close(p[0], 0.);
            close(p[1], 0.);
            close(p[2] / p[3], z);
        }
    }
    #[test]
    fn rebasing_is_equivalent_to_absolute_world_lookup() {
        let value = ShadowMatrices::new(
            [-96., 0., 128.],
            [-0.7808688, 0., 0.6246951],
            0.70710677,
            320.,
        )
        .unwrap();
        let eye = [-190., -200., 190.];
        let point = [-40., 15., 60., 1.];
        let expected = transform(&value.lookup, point);
        let registers = value.lookup_rows(eye);
        let relative = [point[0] - eye[0], point[1] - eye[1], point[2] - eye[2], 1.];
        for row in 0..4 {
            close(
                (0..4).map(|i| registers[row * 4 + i] * relative[i]).sum(),
                expected[row],
            );
        }
    }
    #[test]
    fn caster_transform_keeps_object_origin_and_scale() {
        let value = ShadowMatrices::new([0.; 3], [0., 0., 1.], 0.5, 200.).unwrap();
        let axis = [[1., 0., 0.], [0., 1., 0.], [0., 0., 1.]];
        let registers = value.caster_rows(axis, [10., 20., -40.], 2.);
        let expected = transform(&value.view_projection, [12., 24., -34., 1.]);
        let local = [1., 2., 3., 1.];
        for row in 0..4 {
            close(
                (0..4).map(|i| registers[row * 4 + i] * local[i]).sum(),
                expected[row],
            );
        }
    }
    #[test]
    fn atlas_maps_center_to_first_quarter_and_bias_is_homogeneous_only() {
        let value = ShadowMatrices::new([0.; 3], [0., 0., 1.], 0.5, 200.).unwrap();
        let p = transform(&value.lookup, [0., 0., -10., 1.]);
        close(p[0] / p[3], 0.5 / HOMOGENEOUS_BIAS);
        close(p[1] / p[3], 0.125 / HOMOGENEOUS_BIAS);
        close(value.lookup[10], value.view_projection[10]);
        close(
            value.lookup[11],
            value.view_projection[11] * HOMOGENEOUS_BIAS,
        );
    }
    #[test]
    fn invalid_projection_is_rejected_and_axis_parallel_directions_are_finite() {
        assert!(ShadowMatrices::new([0.; 3], [0.; 3], 0.5, 100.).is_err());
        assert!(ShadowMatrices::new([f32::NAN, 0., 0.], [0., 0., 1.], 0.5, 100.).is_err());
        for direction in [[1., 0., 0.], [0., 1., 0.], [0., 0., 1.]] {
            let value = ShadowMatrices::new([0.; 3], direction, 0.5, 100.).unwrap();
            assert!(value.lookup.iter().all(|v| v.is_finite()));
        }
    }
}
