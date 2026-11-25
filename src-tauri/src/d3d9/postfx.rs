//! Original WaW post-processing programs on D3D9. Only the final ungraded copy
//! uses fixed-function D3D9; no translated GLSL or replacement film shader runs.
use super::{shaders::ShaderLibrary, textures};
use serde_json::Value;
use std::{collections::HashMap, path::Path, ptr::null_mut};
use windows::Win32::Graphics::Direct3D9::*;
type Result<T> = std::result::Result<T, String>;
fn err(e: windows::core::Error) -> String {
    e.to_string()
}
#[derive(Clone)]
struct Target {
    texture: IDirect3DTexture9,
    surface: IDirect3DSurface9,
    width: u32,
    height: u32,
    id: usize,
}
struct Program {
    vs: Option<IDirect3DVertexShader9>,
    ps: IDirect3DPixelShader9,
}
pub struct PostFx {
    device: IDirect3DDevice9,
    library: ShaderLibrary,
    programs: HashMap<String, Program>,
    declaration: IDirect3DVertexDeclaration9,
    full: Option<Target>,
    quarter: Vec<Target>,
    mask: IDirect3DTexture9,
    width: u32,
    height: u32,
}
impl PostFx {
    pub fn new(device: &IDirect3DDevice9, game: &Path) -> Result<Self> {
        unsafe {
            let library = ShaderLibrary::load(game)?;
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
                    Stream: 0,
                    Offset: 12,
                    Type: D3DDECLTYPE_FLOAT2.0 as u8,
                    Method: 0,
                    Usage: D3DDECLUSAGE_TEXCOORD.0 as u8,
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
            let declaration = device
                .CreateVertexDeclaration(elements.as_ptr())
                .map_err(err)?;
            let mask = crate::vision_assets(game)?;
            let mut buffers = HashMap::new();
            buffers.insert(mask.texture.buffer.clone(), mask.bytes);
            let mask = textures::upload(device, &mask.texture, &buffers)?;
            Ok(Self {
                device: device.clone(),
                library,
                programs: HashMap::new(),
                declaration,
                full: None,
                quarter: Vec::new(),
                mask,
                width: 0,
                height: 0,
            })
        }
    }
    unsafe fn target(&self, width: u32, height: u32, id: usize) -> Result<Target> {
        let mut texture = None;
        self.device
            .CreateTexture(
                width,
                height,
                1,
                D3DUSAGE_RENDERTARGET as u32,
                D3DFMT_A8R8G8B8,
                D3DPOOL_DEFAULT,
                &mut texture,
                null_mut(),
            )
            .map_err(err)?;
        let texture = texture.ok_or("No native post target")?;
        let surface = texture.GetSurfaceLevel(0).map_err(err)?;
        Ok(Target {
            texture,
            surface,
            width,
            height,
            id,
        })
    }
    pub fn resize(&mut self, width: u32, height: u32) -> Result<()> {
        if self.width == width && self.height == height {
            return Ok(());
        }
        if width == 0 || height == 0 || width > 8192 || height > 8192 {
            return Err("Invalid native postFX size".into());
        }
        unsafe {
            let full = self.target(width, height, 10)?;
            let mut quarter = Vec::new();
            for i in 0..3 {
                quarter.push(self.target((width / 4).max(1), (height / 4).max(1), i)?);
            }
            self.full = Some(full);
            self.quarter = quarter;
            self.width = width;
            self.height = height;
        }
        Ok(())
    }
    fn program(&mut self, kind: &str, count: u32) -> Result<()> {
        unsafe {
            let key = format!("{kind}:{count}");
            if self.programs.contains_key(&key) {
                return Ok(());
            }
            let name = match kind {
                "film" => "vertcol_film.hlsl".into(),
                "extract" => "glow_consistent_setup.hlsl".into(),
                "apply" => "glow_apply_bloom.hlsl".into(),
                "ray" => "glow_godrays.hlsl".into(),
                "revive" => "lib/revivefx.hlsl".into(),
                "blur" => format!("filter_symmetric_{count}.hlsl"),
                _ => return Err("Unknown native post pass".into()),
            };
            let ps = self
                .device
                .CreatePixelShader(self.library.pixel(&name)?.words.as_ptr())
                .map_err(err)?;
            let vs = if kind == "film" {
                None
            } else {
                Some(
                    self.device
                        .CreateVertexShader(self.library.vertex(&name)?.words.as_ptr())
                        .map_err(err)?,
                )
            };
            self.programs.insert(key, Program { vs, ps });
            Ok(())
        }
    }
    unsafe fn constants(&self, register: u32, values: &[f32], vertex: bool) -> Result<()> {
        if values.len() % 4 != 0 {
            return Err("Post constants need vec4 values".into());
        }
        if vertex {
            self.device.SetVertexShaderConstantF(
                register,
                values.as_ptr(),
                (values.len() / 4) as u32,
            )
        } else {
            self.device.SetPixelShaderConstantF(
                register,
                values.as_ptr(),
                (values.len() / 4) as u32,
            )
        }
        .map_err(err)
    }
    unsafe fn state(
        &self,
        surface: &IDirect3DSurface9,
        width: u32,
        height: u32,
        blend: bool,
    ) -> Result<()> {
        for slot in 0..16 {
            self.device
                .SetTexture(slot, None::<&IDirect3DBaseTexture9>)
                .map_err(err)?;
        }
        self.device.SetRenderTarget(0, surface).map_err(err)?;
        self.device
            .SetDepthStencilSurface(None::<&IDirect3DSurface9>)
            .map_err(err)?;
        self.device
            .SetViewport(&D3DVIEWPORT9 {
                X: 0,
                Y: 0,
                Width: width,
                Height: height,
                MinZ: 0.,
                MaxZ: 1.,
            })
            .map_err(err)?;
        for (state, value) in [
            (D3DRS_ZENABLE, 0),
            (D3DRS_LIGHTING, 0),
            (D3DRS_ZWRITEENABLE, 0),
            (D3DRS_CULLMODE, D3DCULL_NONE.0 as u32),
            (D3DRS_ALPHATESTENABLE, 0),
            (D3DRS_SCISSORTESTENABLE, 0),
            (D3DRS_STENCILENABLE, 0),
            (D3DRS_FOGENABLE, 0),
            (D3DRS_SRGBWRITEENABLE, 0),
            (D3DRS_COLORWRITEENABLE, 15),
            (D3DRS_ALPHABLENDENABLE, blend as u32),
            (D3DRS_SEPARATEALPHABLENDENABLE, 0),
            (D3DRS_BLENDOP, D3DBLENDOP_ADD.0 as u32),
            (D3DRS_SRCBLEND, D3DBLEND_INVDESTCOLOR.0 as u32),
            (D3DRS_DESTBLEND, D3DBLEND_ONE.0 as u32),
        ] {
            self.device.SetRenderState(state, value).map_err(err)?;
        }
        for slot in [0, 4, 5] {
            for (state, value) in [
                (D3DSAMP_ADDRESSU, D3DTADDRESS_CLAMP.0 as u32),
                (D3DSAMP_ADDRESSV, D3DTADDRESS_CLAMP.0 as u32),
                (D3DSAMP_MINFILTER, D3DTEXF_LINEAR.0 as u32),
                (D3DSAMP_MAGFILTER, D3DTEXF_LINEAR.0 as u32),
                (D3DSAMP_MIPFILTER, D3DTEXF_NONE.0 as u32),
                (D3DSAMP_SRGBTEXTURE, 0),
            ] {
                self.device
                    .SetSamplerState(slot, state, value)
                    .map_err(err)?;
            }
        }
        Ok(())
    }
    unsafe fn fixed_quad(&self, width: u32, height: u32) -> Result<()> {
        #[repr(C)]
        struct Vertex {
            p: [f32; 4],
            color: u32,
            uv: [f32; 2],
        }
        let w = width as f32 - 0.5;
        let h = height as f32 - 0.5;
        let q = [
            Vertex {
                p: [-0.5, -0.5, 0., 1.],
                color: 0xffffffff,
                uv: [0., 0.],
            },
            Vertex {
                p: [w, -0.5, 0., 1.],
                color: 0xffffffff,
                uv: [1., 0.],
            },
            Vertex {
                p: [-0.5, h, 0., 1.],
                color: 0xffffffff,
                uv: [0., 1.],
            },
            Vertex {
                p: [w, h, 0., 1.],
                color: 0xffffffff,
                uv: [1., 1.],
            },
        ];
        self.device
            .SetVertexShader(None::<&IDirect3DVertexShader9>)
            .map_err(err)?;
        self.device
            .SetFVF((D3DFVF_XYZRHW | D3DFVF_DIFFUSE | D3DFVF_TEX1) as u32)
            .map_err(err)?;
        self.device
            .DrawPrimitiveUP(
                D3DPT_TRIANGLESTRIP,
                2,
                q.as_ptr().cast(),
                std::mem::size_of::<Vertex>() as u32,
            )
            .map_err(err)
    }
    unsafe fn draw(
        &mut self,
        kind: &str,
        target: &Target,
        source: &IDirect3DTexture9,
        second: Option<&IDirect3DTexture9>,
        third: Option<&IDirect3DTexture9>,
        count: u32,
        parameters: &[f32],
        taps: &[f32],
        ray: &[f32],
        fade: f32,
        blend: bool,
    ) -> Result<()> {
        self.program(kind, count)?;
        self.state(&target.surface, target.width, target.height, blend)?;
        self.device.SetTexture(0, source).map_err(err)?;
        if let Some(image) = second {
            self.device.SetTexture(4, image).map_err(err)?;
        }
        if let Some(image) = third {
            self.device.SetTexture(5, image).map_err(err)?;
        }
        let program = &self.programs[&format!("{kind}:{count}")];
        self.device.SetPixelShader(&program.ps).map_err(err)?;
        match kind {
            "film" => self.constants(5, &parameters[..12], false)?,
            "extract" => {
                self.constants(5, &parameters[24..28], false)?;
                self.constants(6, &parameters[..12], false)?;
                self.constants(
                    16,
                    &[
                        target.width as f32,
                        target.height as f32,
                        1. / target.width as f32,
                        1. / target.height as f32,
                    ],
                    true,
                )?;
            }
            "blur" => {
                self.constants(12, &taps[..count as usize * 4], true)?;
                self.constants(12, &taps[..count as usize * 4], false)?;
            }
            "ray" => self.constants(12, ray, true)?,
            "apply" => self.constants(5, &parameters[28..32], false)?,
            "revive" => {
                self.constants(5, &parameters[..24], false)?;
                self.constants(11, &[fade, fade, fade, fade], false)?;
                self.constants(20, &[0., 0., 1., 1.], true)?;
            }
            _ => {}
        }
        if kind == "film" {
            self.fixed_quad(target.width, target.height)?;
        } else {
            self.device
                .SetVertexShader(program.vs.as_ref())
                .map_err(err)?;
            self.device
                .SetVertexDeclaration(&self.declaration)
                .map_err(err)?;
            self.constants(
                0,
                &[
                    1., 0., 0., 0., 0., 1., 0., 0., 0., 0., 1., 0., 0., 0., 0., 1.,
                ],
                true,
            )?;
            let dx = 1. / target.width as f32;
            let dy = 1. / target.height as f32;
            let q: [[f32; 5]; 4] = [
                [-1. - dx, 1. + dy, 0., 0., 0.],
                [1. - dx, 1. + dy, 0., 1., 0.],
                [-1. - dx, -1. + dy, 0., 0., 1.],
                [1. - dx, -1. + dy, 0., 1., 1.],
            ];
            self.device
                .DrawPrimitiveUP(D3DPT_TRIANGLESTRIP, 2, q.as_ptr().cast(), 20)
                .map_err(err)?;
        }
        Ok(())
    }
    unsafe fn filtered(&mut self, source: Target, plan: &Value) -> Result<Target> {
        let mut image = source;
        for pass in plan.as_array().ok_or("Missing native blur plan")? {
            let count = pass["count"]
                .as_u64()
                .filter(|n| *n >= 1 && *n <= 8)
                .ok_or("Unsupported native blur count")? as u32;
            let taps = vec_floats(&pass["taps"], 32)?;
            let target = self.quarter[if image.id == 0 { 1 } else { 0 }].clone();
            self.draw(
                "blur",
                &target,
                &image.texture,
                None,
                None,
                count,
                &[0.; 32],
                &taps,
                &[0.; 4],
                0.,
                false,
            )?;
            image = target;
        }
        Ok(image)
    }
    pub fn process(
        &mut self,
        source: &IDirect3DTexture9,
        destination: &IDirect3DSurface9,
        view: &Value,
    ) -> Result<()> {
        unsafe {
            let mut desc = D3DSURFACE_DESC::default();
            source
                .GetSurfaceLevel(0)
                .map_err(err)?
                .GetDesc(&mut desc)
                .map_err(err)?;
            self.resize(desc.Width, desc.Height)?;
            self.device.BeginScene().map_err(err)?;
            let result = (|| -> Result<()> {
                let raw = Target {
                    texture: source.clone(),
                    surface: source.GetSurfaceLevel(0).map_err(err)?,
                    width: self.width,
                    height: self.height,
                    id: 99,
                };
                let mut result = raw.clone();
                let v = &view["vision"];
                let values = &v["values"];
                let enabled = v["enabled"].as_bool().unwrap_or(false)
                    && !view["capture"].as_bool().unwrap_or(false);
                let mut parameters = [0.; 32];
                let mut grading = if enabled {
                    vec_floats(&v["grading"], 12)?
                } else {
                    vec![0.; 12]
                };
                if enabled {
                    let revive = values["r_reviveFX_enable"].as_bool().unwrap_or(false)
                        && number(values, "r_reviveFX_blurRadiusEdge", 0.) > 1.;
                    if revive {
                        let blurred = self.filtered(raw.clone(), &v["reviveBlurPlan"])?;
                        grading = vec_floats(&v["reviveEdge"], 12)?;
                        parameters[..12].copy_from_slice(&grading);
                        parameters[12..24].copy_from_slice(&vec_floats(&v["reviveCenter"], 12)?);
                        let target = self.full.as_ref().unwrap().clone();
                        let mask = self.mask.clone();
                        self.draw(
                            "revive",
                            &target,
                            source,
                            Some(&mask),
                            Some(&blurred.texture),
                            1,
                            &parameters,
                            &[0.; 32],
                            &[0.; 4],
                            number(v, "fade", 0.),
                            false,
                        )?;
                        result = target;
                    } else if v["filmActive"].as_bool().unwrap_or(false) {
                        parameters[..12].copy_from_slice(&grading);
                        let target = self.full.as_ref().unwrap().clone();
                        self.draw(
                            "film",
                            &target,
                            source,
                            None,
                            None,
                            1,
                            &parameters,
                            &[0.; 32],
                            &[0.; 4],
                            0.,
                            false,
                        )?;
                        result = target;
                    }
                    if v["glowActive"].as_bool().unwrap_or(false) {
                        parameters[..12].copy_from_slice(&grading);
                        let cutoff = number(values, "r_glowBloomCutoff", 0.);
                        parameters[24..28].copy_from_slice(&[
                            cutoff,
                            1. / (1. - cutoff),
                            0.,
                            number(values, "r_glowBloomDesaturation", 0.),
                        ]);
                        let extraction = self.quarter[0].clone();
                        self.draw(
                            "extract",
                            &extraction,
                            source,
                            None,
                            None,
                            1,
                            &parameters,
                            &[0.; 32],
                            &[0.; 4],
                            0.,
                            false,
                        )?;
                        let mut rays = extraction.clone();
                        let intensity = number(values, "r_glowRayIntensity", 0.)
                            * number(&v["ray"], "visibility", 0.);
                        if intensity != 0. {
                            let centre = vec_floats(&v["ray"]["centre"], 2)?;
                            let expansion = number(values, "r_glowRayExpansion", 0.)
                                * extraction.width as f32
                                / self.width as f32;
                            for pass in 0..4 {
                                let target =
                                    self.quarter[if pass % 2 == 0 { 1 } else { 2 }].clone();
                                self.draw(
                                    "ray",
                                    &target,
                                    &rays.texture,
                                    None,
                                    None,
                                    1,
                                    &parameters,
                                    &[0.; 32],
                                    &[centre[0], centre[1], pass as f32, expansion],
                                    0.,
                                    false,
                                )?;
                                rays = target;
                            }
                        }
                        self.draw(
                            "extract",
                            &extraction,
                            source,
                            None,
                            None,
                            1,
                            &parameters,
                            &[0.; 32],
                            &[0.; 4],
                            0.,
                            false,
                        )?;
                        let bloom = self.filtered(extraction, &v["blurPlan"])?;
                        parameters[28..32].copy_from_slice(&[
                            0.,
                            0.,
                            intensity,
                            number(values, "r_glowBloomIntensity0", 0.),
                        ]);
                        self.draw(
                            "apply",
                            &result,
                            &bloom.texture,
                            Some(&rays.texture),
                            None,
                            1,
                            &parameters,
                            &[0.; 32],
                            &[0.; 4],
                            0.,
                            true,
                        )?;
                    }
                }
                let mut output = D3DSURFACE_DESC::default();
                destination.GetDesc(&mut output).map_err(err)?;
                self.state(destination, output.Width, output.Height, false)?;
                self.device
                    .SetPixelShader(None::<&IDirect3DPixelShader9>)
                    .map_err(err)?;
                self.device.SetTexture(0, &result.texture).map_err(err)?;
                self.device
                    .SetTextureStageState(0, D3DTSS_COLOROP, D3DTOP_SELECTARG1.0 as u32)
                    .map_err(err)?;
                self.device
                    .SetTextureStageState(0, D3DTSS_COLORARG1, D3DTA_TEXTURE)
                    .map_err(err)?;
                self.device
                    .SetTextureStageState(0, D3DTSS_ALPHAOP, D3DTOP_SELECTARG1.0 as u32)
                    .map_err(err)?;
                self.device
                    .SetTextureStageState(0, D3DTSS_ALPHAARG1, D3DTA_TEXTURE)
                    .map_err(err)?;
                self.device
                    .SetTextureStageState(1, D3DTSS_COLOROP, D3DTOP_DISABLE.0 as u32)
                    .map_err(err)?;
                self.fixed_quad(output.Width, output.Height)?;
                Ok(())
            })();
            let end = self.device.EndScene().map_err(err);
            result.and(end)
        }
    }
}
fn number(value: &Value, key: &str, default: f32) -> f32 {
    value[key]
        .as_f64()
        .filter(|v| v.is_finite())
        .map(|v| v as f32)
        .unwrap_or(default)
}
fn vec_floats(value: &Value, count: usize) -> Result<Vec<f32>> {
    let a = value.as_array().ok_or("Missing native postFX vector")?;
    if a.len() < count {
        return Err("Native postFX vector is short".into());
    }
    a[..count]
        .iter()
        .map(|x| {
            x.as_f64()
                .filter(|v| v.is_finite())
                .map(|v| v as f32)
                .ok_or("Invalid native postFX number".into())
        })
        .collect()
}
