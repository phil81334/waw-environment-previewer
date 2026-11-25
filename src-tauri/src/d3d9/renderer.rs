//! Native scene rendering with the user's original D3D9 shader programs.
//! Uploads compiled scene data and renders it with the installed shader programs.
use super::{
    adapter::Adapter,
    float_z::FloatZ,
    postfx::PostFx,
    scene_fx::{FxCamera, NativeSceneFx},
    shaders::{ShaderLibrary, ShaderProgram},
    shadows::{self, DynamicShadow, ShadowMatrices},
    sky::{PreviewClock, SkyMaterial},
    textures,
};
use crate::{BakedScene, Light, Material, Texture};
use serde_json::{json, Value};
use std::{
    collections::{HashMap, HashSet},
    path::{Path, PathBuf},
    ptr::{copy_nonoverlapping, null, null_mut},
    time::Instant,
};
use windows::Win32::{Foundation::HWND, Graphics::Direct3D9::*};
type Result<T> = std::result::Result<T, String>;
fn err(e: windows::core::Error) -> String {
    e.to_string()
}
fn number(v: &Value, default: f32) -> f32 {
    v.as_f64()
        .filter(|v| v.is_finite())
        .map(|v| v as f32)
        .unwrap_or(default)
}
fn vector<const N: usize>(v: &Value, default: [f32; N]) -> [f32; N] {
    let mut out = default;
    if let Some(a) = v.as_array() {
        if a.len() == N {
            for i in 0..N {
                out[i] = number(&a[i], default[i]);
            }
        }
    }
    out
}
fn f32_at(b: &[u8], p: usize) -> Result<f32> {
    let value = f32::from_le_bytes(
        b.get(p..p + 4)
            .ok_or("Truncated mesh float")?
            .try_into()
            .unwrap(),
    );
    if !value.is_finite() {
        return Err("Nonfinite mesh coordinate".into());
    }
    Ok(value)
}
fn buffer<'a>(scene: &'a BakedScene, id: &str) -> Result<&'a [u8]> {
    scene
        .buffers
        .get(id)
        .map(Vec::as_slice)
        .ok_or_else(|| format!("Missing scene buffer {id}"))
}
fn basis(v: [f32; 3]) -> [u8; 4] {
    let mut p = [0, 0, 0, 63];
    for i in 0..3 {
        p[i] = ((v[i] * 127.0 + 127.5) as i32 & 255) as u8;
    }
    p
}
fn half(value: f32) -> u16 {
    let bits = value.to_bits();
    let sign = ((bits >> 16) & 0x8000) as u16;
    let e = ((bits >> 23) & 255) as i32 - 127 + 15;
    let m = bits & 0x7fffff;
    if e >= 31 {
        return sign | 0x7bff;
    }
    if e <= 0 {
        if e < -10 {
            return sign;
        }
        let v = m | 0x800000;
        let shift = (14 - e) as u32;
        let q = v >> shift;
        let r = v & ((1 << shift) - 1);
        let mid = 1 << (shift - 1);
        return sign | (q + u32::from(r > mid || (r == mid && q & 1 != 0))) as u16;
    }
    sign | (((e as u32) << 10) + ((m + 0xfff + ((m >> 13) & 1)) >> 13)) as u16
}
fn append_vertex(
    out: &mut Vec<u8>,
    position: [f32; 3],
    sign: f32,
    color: [u8; 4],
    uv: [f32; 4],
    normal: [u8; 4],
    tangent: [u8; 4],
) {
    for x in position.into_iter().chain([sign]) {
        out.extend(x.to_le_bytes())
    }
    out.extend([color[2], color[1], color[0], color[3]]);
    for x in uv {
        out.extend(x.to_le_bytes())
    }
    out.extend(normal);
    out.extend(tangent);
}
fn world_vertices(input: &[u8]) -> Result<Vec<u8>> {
    if input.len() % 44 != 0 {
        return Err("World mesh stride invalid".into());
    }
    let mut out = Vec::with_capacity(input.len());
    for v in input.chunks_exact(44) {
        append_vertex(
            &mut out,
            [f32_at(v, 0)?, f32_at(v, 4)?, f32_at(v, 8)?],
            f32_at(v, 40)?,
            v[12..16].try_into().unwrap(),
            [
                f32_at(v, 16)?,
                f32_at(v, 20)?,
                f32_at(v, 24)?,
                f32_at(v, 28)?,
            ],
            v[32..36].try_into().unwrap(),
            v[36..40].try_into().unwrap(),
        );
    }
    Ok(out)
}

/// Validate the optional world mesh before any GPU allocation. A model-only
/// candidate may retain packed vertices from omitted compiler surfaces, but it
/// must have zero world indices and no world draws. Missing zero-count buffers
/// are accepted; missing buffers with nonzero declared counts remain fatal.
fn optional_world_mesh(baked: &BakedScene) -> Result<Option<(Vec<u8>, Vec<u8>)>> {
    let m = &baked.manifest;
    let g = &m.geometry;
    if g.stride != 44 || g.index_type != "uint32" {
        return Err("Invalid native world geometry layout".into());
    }
    let vertices = match baked.buffers.get(&g.vertex_buffer) {
        Some(bytes) => bytes.as_slice(),
        None if g.vertex_count == 0 => &[],
        None => return Err(format!("Missing scene buffer {}", g.vertex_buffer)),
    };
    let indices = match baked.buffers.get(&g.index_buffer) {
        Some(bytes) => bytes.as_slice(),
        None if g.index_count == 0 => &[],
        None => return Err(format!("Missing scene buffer {}", g.index_buffer)),
    };
    if g.vertex_count.checked_mul(44) != Some(vertices.len())
        || g.index_count.checked_mul(4) != Some(indices.len())
        || g.index_count % 3 != 0
    {
        return Err("Native world geometry counts disagree with buffers".into());
    }
    // Check packed coordinates even for omitted world surfaces; skipping the
    // GPU mesh is not permission to accept malformed retained bytes.
    let converted = world_vertices(vertices)?;
    for value in indices.chunks_exact(4) {
        if u32::from_le_bytes(value.try_into().unwrap()) as usize >= g.vertex_count {
            return Err("Native world mesh index out of bounds".into());
        }
    }
    if g.index_count == 0 {
        if !m.draws.is_empty() {
            return Err("World draws reference an empty native world mesh".into());
        }
        return Ok(None);
    }
    if m.draws.is_empty() {
        return Err("Nonempty native world mesh has no draws".into());
    }
    for draw in &m.draws {
        if draw.index_count == 0
            || draw.index_count % 3 != 0
            || draw
                .first_index
                .checked_add(draw.index_count)
                .is_none_or(|end| end > g.index_count)
            || draw.lightmap_index >= m.lightmaps.len()
        {
            return Err("Native world draw references invalid geometry or lighting".into());
        }
    }
    Ok(Some((converted, indices.to_vec())))
}

fn model_vertices(input: &[u8], raw_uv: bool) -> Result<Vec<u8>> {
    if input.len() % 52 != 0 {
        return Err("Model mesh stride invalid".into());
    }
    let mut out = Vec::with_capacity(input.len() / 52 * 44);
    for v in input.chunks_exact(52) {
        let u = half(f32_at(v, 40)?).to_le_bytes();
        let w = half(f32_at(v, 44)?).to_le_bytes();
        append_vertex(
            &mut out,
            [f32_at(v, 0)?, f32_at(v, 4)?, f32_at(v, 8)?],
            f32_at(v, 36)?,
            v[48..52].try_into().unwrap(),
            if raw_uv {
                [f32_at(v, 40)?, f32_at(v, 44)?, 0.0, 0.0]
            } else {
                [w[0] as f32, w[1] as f32, u[0] as f32, u[1] as f32]
            },
            basis([f32_at(v, 12)?, f32_at(v, 16)?, f32_at(v, 20)?]),
            basis([f32_at(v, 24)?, f32_at(v, 28)?, f32_at(v, 32)?]),
        );
    }
    Ok(out)
}
fn sky_vertices(input: &[u8]) -> Result<Vec<u8>> {
    if input.len() % 44 != 0 {
        return Err("Sky mesh stride invalid".into());
    }
    let mut out = Vec::with_capacity(input.len());
    for v in input.chunks_exact(44) {
        let mut color = [0; 4];
        for i in 0..4 {
            color[i] = (f32_at(v, 12 + i * 4)?.clamp(0.0, 1.0) * 255.0 + 0.5) as u8;
        }
        append_vertex(
            &mut out,
            [f32_at(v, 0)?, f32_at(v, 4)?, f32_at(v, 8)?],
            1.0,
            color,
            [
                f32_at(v, 28)?,
                f32_at(v, 32)?,
                f32_at(v, 36)?,
                f32_at(v, 40)?,
            ],
            [127, 127, 254, 63],
            [254, 127, 127, 63],
        );
    }
    Ok(out)
}
#[derive(Clone)]
struct Program {
    vertex: IDirect3DVertexShader9,
    pixel: IDirect3DPixelShader9,
    vertex_metadata: ShaderProgram,
    pixel_metadata: ShaderProgram,
}
struct Mesh {
    vertices: IDirect3DVertexBuffer9,
    indices: IDirect3DIndexBuffer9,
    vertex_count: u32,
    index_count: u32,
}
#[derive(Clone, Copy, PartialEq)]
enum Kind {
    World,
    Model,
    Sky,
}
struct Draw {
    mesh: usize,
    first: u32,
    count: u32,
    material: Material,
    program: Program,
    shadow_program: Option<Program>,
    kind: Kind,
    object: Option<String>,
    origin: [f32; 3],
    axis: [[f32; 3]; 3],
    scale: f32,
    lightmap: usize,
    light: Option<Light>,
    probe: usize,
    volume: Option<IDirect3DVolumeTexture9>,
    scroll: [f32; 2],
    sky_material: Option<SkyMaterial>,
}
struct Scene {
    source_id: String,
    meshes: Vec<Mesh>,
    draws: Vec<Draw>,
    textures: HashMap<String, IDirect3DTexture9>,
    lightmaps: Vec<(IDirect3DTexture9, IDirect3DTexture9)>,
    falloffs: HashMap<String, [f32; 4]>,
    cubes: Vec<IDirect3DCubeTexture9>,
    warnings: Vec<String>,
    model_instances: usize,
    model_assets: usize,
    lights: Vec<Light>,
}
struct Frame {
    texture: IDirect3DTexture9,
    surface: IDirect3DSurface9,
    depth: IDirect3DSurface9,
    width: u32,
    height: u32,
}
impl Frame {
    unsafe fn new(device: &IDirect3DDevice9, width: u32, height: u32) -> Result<Self> {
        let mut texture = None;
        device
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
        let texture = texture.ok_or("No frame texture")?;
        let surface = texture.GetSurfaceLevel(0).map_err(err)?;
        let mut depth = None;
        device
            .CreateDepthStencilSurface(
                width,
                height,
                D3DFMT_D24S8,
                D3DMULTISAMPLE_NONE,
                0,
                true,
                &mut depth,
                null_mut(),
            )
            .map_err(err)?;
        Ok(Self {
            texture,
            surface,
            depth: depth.ok_or("No depth target")?,
            width,
            height,
        })
    }
}

/// All native D3D resources are created, used and destroyed on the UI thread.
pub struct Dx9Renderer {
    device: IDirect3DDevice9,
    adapter: Adapter,
    dirty: bool,
    hwnd: HWND,
    library: ShaderLibrary,
    programs: HashMap<String, Program>,
    declaration: IDirect3DVertexDeclaration9,
    scene: Option<Scene>,
    view: Value,
    hidden: HashSet<String>,
    frame: Option<Frame>,
    output: Option<Frame>,
    swapchain: Option<IDirect3DSwapChain9>,
    width: u32,
    height: u32,
    post: PostFx,
    white: IDirect3DTexture9,
    black: IDirect3DTexture9,
    normal: IDirect3DTexture9,
    black_cube: IDirect3DCubeTexture9,
    last_error: Option<String>,
    shadow: Option<DynamicShadow>,
    shadow_warning: Option<String>,
    shadow_matrices: Option<ShadowMatrices>,
    game: PathBuf,
    fx: Option<NativeSceneFx>,
    fx_warning: Option<String>,
    fx_tick: Instant,
    float_z: Option<FloatZ>,
}
impl Dx9Renderer {
    pub fn new(hwnd: isize, game: &Path) -> Result<Self> {
        unsafe {
            let hwnd = HWND(hwnd as *mut _);
            let factory = Direct3DCreate9(D3D_SDK_VERSION).ok_or("Direct3D9 is unavailable")?;
            let devices = super::adapter::enumerate(&factory)?;
            let mut failures = Vec::new();
            for adapter in super::adapter::candidates(&devices)? {
                match Self::on_adapter(hwnd, game, &factory, adapter.clone()) {
                    Ok(renderer) => return Ok(renderer),
                    Err(error) => failures.push(format!("{}: {error}", adapter.description)),
                }
            }
            Err(format!(
                "Direct3D 9 renderer setup failed. {}",
                failures.join("; ")
            ))
        }
    }

    fn on_adapter(hwnd: HWND, game: &Path, factory: &IDirect3D9, adapter: Adapter) -> Result<Self> {
        unsafe {
            let mut parameters = D3DPRESENT_PARAMETERS {
                BackBufferWidth: 8,
                BackBufferHeight: 8,
                BackBufferFormat: D3DFMT_A8R8G8B8,
                BackBufferCount: 1,
                SwapEffect: D3DSWAPEFFECT_DISCARD,
                hDeviceWindow: hwnd,
                Windowed: true.into(),
                PresentationInterval: D3DPRESENT_INTERVAL_IMMEDIATE as u32,
                ..Default::default()
            };
            let mut device = None;
            factory
                .CreateDevice(
                    adapter.ordinal,
                    D3DDEVTYPE_HAL,
                    hwnd,
                    (D3DCREATE_HARDWARE_VERTEXPROCESSING | D3DCREATE_FPU_PRESERVE) as u32,
                    &mut parameters,
                    &mut device,
                )
                .map_err(err)?;
            let device = device.ok_or("No D3D9 device")?;
            let element = |offset, kind, usage, index| D3DVERTEXELEMENT9 {
                Stream: 0,
                Offset: offset,
                Type: kind,
                Method: 0,
                Usage: usage,
                UsageIndex: index,
            };
            let elements = [
                element(0, 3, 0, 0),
                element(16, 4, 10, 0),
                element(20, 3, 5, 0),
                element(36, 5, 3, 0),
                element(40, 5, 5, 2),
                D3DVERTEXELEMENT9 {
                    Stream: 255,
                    Offset: 0,
                    Type: 17,
                    Method: 0,
                    Usage: 0,
                    UsageIndex: 0,
                },
            ];
            let declaration = device
                .CreateVertexDeclaration(elements.as_ptr())
                .map_err(err)?;
            let white = textures::raw(&device, 1, 1, "BGRA8", &[255, 255, 255, 255])?;
            let black = textures::raw(&device, 1, 1, "BGRA8", &[0, 0, 0, 0])?;
            let normal = textures::raw(&device, 1, 1, "RGBA8", &[128, 128, 255, 128])?;
            let black_cube = textures::black_cube(&device)?;
            let library = ShaderLibrary::load(game)?;
            let post = PostFx::new(&device, game)?;
            let shadow = DynamicShadow::new(&device, &library);
            let shadow_warning = shadow.as_ref().err().cloned();
            Ok(Self {
                device,
                adapter,
                dirty: true,
                hwnd,
                library,
                programs: HashMap::new(),
                declaration,
                scene: None,
                view: json!({}),
                hidden: HashSet::new(),
                frame: None,
                output: None,
                swapchain: None,
                width: 8,
                height: 8,
                post,
                white,
                black,
                normal,
                black_cube,
                last_error: None,
                shadow: shadow.ok(),
                shadow_warning,
                shadow_matrices: None,
                game: game.to_path_buf(),
                fx: None,
                fx_warning: None,
                fx_tick: Instant::now(),
                float_z: None,
            })
        }
    }
    fn program(&mut self, vs: &str, ps: &str) -> Result<Program> {
        let key = format!("{vs}|{ps}");
        if let Some(p) = self.programs.get(&key) {
            return Ok(p.clone());
        }
        let vertex_metadata = self.library.vertex(vs)?.clone();
        let pixel_metadata = self.library.pixel(ps)?.clone();
        unsafe {
            let vertex = self
                .device
                .CreateVertexShader(vertex_metadata.words.as_ptr())
                .map_err(err)?;
            let pixel = self
                .device
                .CreatePixelShader(pixel_metadata.words.as_ptr())
                .map_err(err)?;
            let program = Program {
                vertex,
                pixel,
                vertex_metadata,
                pixel_metadata,
            };
            self.programs.insert(key, program.clone());
            Ok(program)
        }
    }
    pub fn resize(&mut self, width: u32, height: u32) -> Result<()> {
        self.dirty = true;
        if width == 0 || height == 0 || width > 8192 || height > 8192 {
            return Err("Invalid native viewport size".into());
        }
        if self.width == width && self.height == height && self.swapchain.is_some() {
            return Ok(());
        }
        unsafe {
            let mut parameters = D3DPRESENT_PARAMETERS {
                BackBufferWidth: width,
                BackBufferHeight: height,
                BackBufferFormat: D3DFMT_A8R8G8B8,
                BackBufferCount: 1,
                SwapEffect: D3DSWAPEFFECT_DISCARD,
                hDeviceWindow: self.hwnd,
                Windowed: true.into(),
                PresentationInterval: D3DPRESENT_INTERVAL_IMMEDIATE as u32,
                ..Default::default()
            };
            let mut chain = None;
            self.device
                .CreateAdditionalSwapChain(&mut parameters, &mut chain)
                .map_err(err)?;
            self.swapchain = Some(chain.ok_or("No native swapchain")?);
        }
        self.width = width;
        self.height = height;
        Ok(())
    }
    pub fn set_view(&mut self, view: Value) -> Result<()> {
        if !view.is_object() {
            return Err("Native view must be an object".into());
        }
        if self.view != view {
            PreviewClock::from_view(&view)?;
            if let Some(fx) = self.fx.as_mut() {
                fx.set_controls(&view["fx"])?;
            }
            self.dirty = true;
            self.view = view;
        }
        Ok(())
    }
    /// CPU preparation happens off the UI thread; GPU resources commit only
    /// after the candidate has loaded. A failed effect never replaces the map.
    pub fn load_fx(&mut self, prepared: &crate::scene_fx::PreparedEffect) -> Result<Value> {
        let mut candidate = NativeSceneFx::new(&self.device, &self.game)?;
        let metadata = candidate.load(prepared)?;
        candidate.set_controls(&self.view["fx"])?;
        if self.float_z.is_none() {
            self.float_z = Some(FloatZ::new(&self.device, &self.library)?);
        }
        self.fx = Some(candidate);
        self.fx_warning = None;
        self.fx_tick = Instant::now();
        self.dirty = true;
        Ok(metadata)
    }
    pub fn clear_fx(&mut self) -> Result<()> {
        self.fx = None;
        self.fx_warning = None;
        self.dirty = true;
        Ok(())
    }
    fn effect_camera(&self, width: u32, height: u32) -> FxCamera {
        let eye = vector(&self.view["camera"]["position"], [0.0; 3]);
        let yaw = number(&self.view["camera"]["yaw"], 0.0);
        let pitch = number(&self.view["camera"]["pitch"], 0.0);
        let right = [yaw.sin(), -yaw.cos(), 0.0];
        let up = [
            -pitch.sin() * yaw.cos(),
            -pitch.sin() * yaw.sin(),
            pitch.cos(),
        ];
        let forward = [
            pitch.cos() * yaw.cos(),
            pitch.cos() * yaw.sin(),
            pitch.sin(),
        ];
        let relative_rows = transpose(vector(
            &self.view["viewProjection"],
            camera_matrix(&self.view["camera"], width as f32 / height as f32),
        ));
        let mut rows = relative_rows;
        for row in 0..4 {
            rows[row * 4 + 3] -= (0..3).map(|i| rows[row * 4 + i] * eye[i]).sum::<f32>();
        }
        let mut view_rows = [0.0; 16];
        for (row, axis) in [right, up, forward].iter().enumerate() {
            view_rows[row * 4..row * 4 + 3].copy_from_slice(axis);
            view_rows[row * 4 + 3] = -(0..3).map(|i| axis[i] * eye[i]).sum::<f32>();
        }
        view_rows[15] = 1.0;
        let a = 2047.0 / 2048.0;
        let focal = a / (number(&self.view["camera"]["fov"], 51.077194).to_radians() * 0.5).tan();
        let projection_rows = [
            focal / (width as f32 / height as f32),
            0.0,
            0.0,
            0.0,
            0.0,
            focal,
            0.0,
            0.0,
            0.0,
            0.0,
            a,
            -4.0 * a,
            0.0,
            0.0,
            1.0,
            0.0,
        ];
        FxCamera {
            origin: eye,
            axis: [forward, right.map(|value| -value), up],
            view_projection_rows: rows,
            relative_view_projection_rows: relative_rows,
            view_rows,
            projection_rows,
            width,
            height,
        }
    }
    fn omit_fx(&mut self, error: String) -> Result<()> {
        // Content errors are separable. A lost device remains fatal and stops
        // the native host's draw loop instead of repeatedly touching the driver.
        unsafe {
            self.device.TestCooperativeLevel().map_err(err)?;
        }
        self.fx = None;
        self.fx_warning = Some(format!("FX omitted after runtime failure: {error}"));
        self.dirty = true;
        Ok(())
    }
    pub fn set_object_visibility(&mut self, id: &str, visible: bool) {
        self.dirty = true;
        if visible {
            self.hidden.remove(id);
        } else if id.len() <= 128 {
            self.hidden.insert(id.to_owned());
        }
    }
    unsafe fn mesh(&self, vertices: &[u8], indices: &[u8]) -> Result<Mesh> {
        if vertices.len() % 44 != 0
            || indices.len() % 12 != 0
            || vertices.is_empty()
            || indices.is_empty()
        {
            return Err("Invalid native mesh buffers".into());
        }
        let count = vertices.len() / 44;
        for p in indices.chunks_exact(4) {
            if u32::from_le_bytes(p.try_into().unwrap()) as usize >= count {
                return Err("Native mesh index out of bounds".into());
            }
        }
        let mut vb = None;
        self.device
            .CreateVertexBuffer(
                vertices.len() as u32,
                D3DUSAGE_WRITEONLY as u32,
                0,
                D3DPOOL_MANAGED,
                &mut vb,
                null_mut(),
            )
            .map_err(err)?;
        let vb = vb.ok_or("No vertex buffer")?;
        let mut ptr = null_mut();
        vb.Lock(0, vertices.len() as u32, &mut ptr, 0)
            .map_err(err)?;
        copy_nonoverlapping(vertices.as_ptr(), ptr.cast(), vertices.len());
        vb.Unlock().map_err(err)?;
        let mut ib = None;
        self.device
            .CreateIndexBuffer(
                indices.len() as u32,
                D3DUSAGE_WRITEONLY as u32,
                D3DFMT_INDEX32,
                D3DPOOL_MANAGED,
                &mut ib,
                null_mut(),
            )
            .map_err(err)?;
        let ib = ib.ok_or("No index buffer")?;
        ib.Lock(0, indices.len() as u32, &mut ptr, 0).map_err(err)?;
        copy_nonoverlapping(indices.as_ptr(), ptr.cast(), indices.len());
        ib.Unlock().map_err(err)?;
        Ok(Mesh {
            vertices: vb,
            indices: ib,
            vertex_count: count as u32,
            index_count: (indices.len() / 4) as u32,
        })
    }
    pub fn load_scene(&mut self, baked: &BakedScene) -> Result<()> {
        unsafe {
            let m = &baked.manifest;
            let world_mesh = optional_world_mesh(baked)?;
            let mut next = Scene {
                source_id: m.id.clone(),
                meshes: Vec::new(),
                draws: Vec::new(),
                textures: HashMap::new(),
                lightmaps: Vec::new(),
                falloffs: HashMap::new(),
                cubes: Vec::new(),
                warnings: Vec::new(),
                model_instances: 0,
                model_assets: 0,
                lights: m.lights.clone(),
            };
            for meta in &m.textures {
                next.textures.insert(
                    meta.id.clone(),
                    textures::upload(&self.device, meta, &baked.buffers)?,
                );
            }
            let mut x = 0u32;
            for def in &m.falloffs {
                if let Some(meta) = m.textures.iter().find(|t| t.id == def.texture_id) {
                    if meta.height != 1 || x + meta.width + 2 > 512 {
                        return Err("Unsupported falloff atlas".into());
                    }
                    next.falloffs.insert(
                        def.definition.clone(),
                        [meta.width as f32 / 512.0, 0.0, (x + 1) as f32 / 512.0, 0.0],
                    );
                    x += meta.width + 2;
                }
            }
            for map in &m.lightmaps {
                let mut secondary = buffer(baked, &map.secondary_buffer)?.to_vec();
                for def in &m.falloffs {
                    if let (Some(meta), Some(place)) = (
                        m.textures.iter().find(|t| t.id == def.texture_id),
                        next.falloffs.get(&def.definition),
                    ) {
                        let pix = buffer(baked, &meta.buffer)?;
                        let start = (place[2] * 512.0) as usize;
                        let width = meta.width as usize;
                        let src = textures::falloff_row(&meta.format, width, pix)?;
                        secondary[(start - 1) * 4..start * 4].copy_from_slice(&src[..4]);
                        secondary[start * 4..(start + width) * 4].copy_from_slice(&src);
                        secondary[(start + width) * 4..(start + width + 1) * 4]
                            .copy_from_slice(&src[(width - 1) * 4..]);
                    }
                }
                next.lightmaps.push((
                    textures::raw(
                        &self.device,
                        map.primary_width,
                        map.primary_height,
                        "R8",
                        buffer(baked, &map.primary_buffer)?,
                    )?,
                    textures::raw(&self.device, map.width, map.height, &map.format, &secondary)?,
                ));
            }
            let world_mesh_index = if let Some((vertices, indices)) = world_mesh {
                let index = next.meshes.len();
                next.meshes.push(self.mesh(&vertices, &indices)?);
                Some(index)
            } else {
                next.warnings.push(
                    "No supported world mesh; retaining supported placed-model draws.".into(),
                );
                None
            };
            for draw in &m.draws {
                let mat = m
                    .materials
                    .get(draw.material_index)
                    .ok_or("Invalid world material")?
                    .clone();
                let light = m
                    .lights
                    .iter()
                    .find(|v| v.index == draw.primary_light_index && v.r#type > 0)
                    .cloned();
                let prefix = match light.as_ref().map(|v| v.r#type) {
                    Some(1) => "lm_sun",
                    Some(2) => "lm_spot",
                    Some(3) => "lm_omni",
                    _ => "lm",
                };
                let no_specular = mat.techset == "l_sm_r0c0n0";
                let family = if no_specular { "r0c0n0" } else { "r0c0n0s0" };
                let vs = if no_specular {
                    match light.as_ref().map(|value| value.r#type) {
                        Some(1) => "lm_sun_tc0n0_sm3.hlsl",
                        Some(2 | 3) => "lm_omni_tc0n0_sm3.hlsl",
                        _ => "lm_tc0n0_sm3.hlsl",
                    }
                } else {
                    "lm_s_tc0n0_sm3.hlsl"
                };
                let ps = format!("{prefix}_{family}_sm3.hlsl");
                let program = self.program(vs, &ps)?;
                // Research proves static baked receiver programs only. A matching
                // no-spec shadow variant must be tested before dynamic shadow use.
                let shadow_program =
                    if !no_specular && light.as_ref().is_some_and(shadows::eligible) {
                        let kind = if light.as_ref().unwrap().r#type == 2 {
                            "spot"
                        } else {
                            "omni"
                        };
                        match self.program(
                            "lm_hsm_omni_s_tc0n0_sm3.hlsl",
                            &format!("lm_hsm_{kind}_r0c0n0s0_sm3.hlsl"),
                        ) {
                            Ok(value) => Some(value),
                            Err(error) => {
                                next.warnings
                                    .push(format!("World shadow receiver unavailable: {error}"));
                                None
                            }
                        }
                    } else {
                        None
                    };
                next.draws.push(Draw {
                    mesh: world_mesh_index.ok_or("World draw has no native mesh")?,
                    first: draw.first_index as u32,
                    count: draw.index_count as u32,
                    material: mat,
                    program,
                    shadow_program,
                    kind: Kind::World,
                    object: draw.object_id.clone(),
                    origin: [0.0; 3],
                    axis: [[1., 0., 0.], [0., 1., 0.], [0., 0., 1.]],
                    scale: 1.0,
                    lightmap: draw.lightmap_index,
                    light,
                    probe: draw.probe_index,
                    volume: None,
                    scroll: [0.0; 2],
                    sky_material: None,
                });
            }
            if let Some(models) = &m.models {
                let assets = models["assets"].as_array().ok_or("Invalid model assets")?;
                let mut entries = HashMap::new();
                for asset in assets {
                    let id = asset["id"]
                        .as_str()
                        .or_else(|| asset["name"].as_str())
                        .ok_or("Model asset has no id")?
                        .to_owned();
                    let mut ids = Vec::new();
                    for surface in asset["surfaces"]
                        .as_array()
                        .ok_or("Model asset has no surfaces")?
                    {
                        let vertices = buffer(
                            baked,
                            surface["vertexBuffer"]
                                .as_str()
                                .ok_or("Model vertex buffer missing")?,
                        )?;
                        let indices = buffer(
                            baked,
                            surface["indexBuffer"]
                                .as_str()
                                .ok_or("Model index buffer missing")?,
                        )?;
                        let mat: Material = serde_json::from_value(surface["material"].clone())
                            .map_err(|e| format!("Model material: {e}"))?;
                        let stem = mat.techset.strip_prefix("l_sm_").unwrap_or("");
                        let stem = stem
                            .split("_clothcharr")
                            .next()
                            .unwrap_or(stem)
                            .split("_sco")
                            .next()
                            .unwrap_or(stem);
                        let (vs, ps) = if mat.techset == "objective" {
                            (
                                "objective_base_dtex.hlsl".to_owned(),
                                "objective_base.hlsl".to_owned(),
                            )
                        } else if mat.techset == "unlit" || mat.techset == "unlit_sm" {
                            (
                                "vertcol_simple_fog_dtex.hlsl".to_owned(),
                                "vertcol_simple_fog.hlsl".to_owned(),
                            )
                        } else {
                            (
                                if mat.techset.ends_with("_sco") {
                                    "lp_s_tc0n0_dtex_sm3_sco.hlsl".to_owned()
                                } else {
                                    "lp_s_tc0n0_dtex_sm3.hlsl".to_owned()
                                },
                                if mat.techset.ends_with("_sco") {
                                    format!("lp_{stem}_sm3_sco.hlsl")
                                } else {
                                    format!("lp_{stem}_sm3.hlsl")
                                },
                            )
                        };
                        let program = match self.program(&vs, &ps) {
                            Ok(p) => p,
                            Err(e) => {
                                next.warnings
                                    .push(format!("Model {id} material {} omitted: {e}", mat.name));
                                continue;
                            }
                        };
                        let mesh = next.meshes.len();
                        next.meshes.push(self.mesh(
                            &model_vertices(vertices, mat.techset == "objective")?,
                            indices,
                        )?);
                        ids.push((mesh, mat, program));
                    }
                    entries.insert(id, ids);
                }
                next.model_assets = entries.len();
                for instance in models["instances"]
                    .as_array()
                    .ok_or("Missing model instances")?
                {
                    let id = instance["id"]
                        .as_str()
                        .ok_or("Model instance has no id")?
                        .to_owned();
                    let asset = instance["assetId"]
                        .as_str()
                        .ok_or("Model instance has no asset")?;
                    let Some(surfaces) = entries.get(asset) else {
                        continue;
                    };
                    let origin = vector(&instance["origin"], [0.0; 3]);
                    let scale = number(&instance["scale"], 1.0);
                    let ax = &instance["axis"];
                    let axis = if ax.as_array().is_some_and(|a| a.len() == 9) {
                        let q = vector::<9>(ax, [1., 0., 0., 0., 1., 0., 0., 0., 1.]);
                        [[q[0], q[1], q[2]], [q[3], q[4], q[5]], [q[6], q[7], q[8]]]
                    } else {
                        [
                            vector(&ax[0], [1., 0., 0.]),
                            vector(&ax[1], [0., 1., 0.]),
                            vector(&ax[2], [0., 0., 1.]),
                        ]
                    };
                    let light = m
                        .lights
                        .iter()
                        .find(|v| {
                            Some(v.index as u64) == instance["primaryLightIndex"].as_u64()
                                && v.r#type > 0
                        })
                        .cloned();
                    let volume = instance["lightingVolumeBuffer"]
                        .as_str()
                        .map(|id| buffer(baked, id).and_then(|b| textures::volume(&self.device, b)))
                        .transpose()?;
                    let probe = instance["probeIndex"].as_u64().unwrap_or(0) as usize;
                    for (mesh, mat, program) in surfaces {
                        let mut program = program.clone();
                        let direct = instance["lightingVolumeBuffer"]
                            .as_str()
                            .and_then(|id| baked.buffers.get(id))
                            .is_some_and(|b| b.chunks_exact(4).any(|p| p[3] > 0));
                        if direct && mat.techset.starts_with("l_sm_") {
                            if let Some(light) = &light {
                                let prefix = match light.r#type {
                                    1 => "lp_sun",
                                    2 => "lp_spot",
                                    3 => "lp_omni",
                                    _ => "lp",
                                };
                                let stem = mat
                                    .techset
                                    .strip_prefix("l_sm_")
                                    .unwrap_or("")
                                    .split("_clothcharr")
                                    .next()
                                    .unwrap_or("")
                                    .split("_sco")
                                    .next()
                                    .unwrap_or("");
                                let name = if mat.techset.ends_with("_sco") {
                                    format!("{prefix}_{stem}_sm3_sco.hlsl")
                                } else {
                                    format!("{prefix}_{stem}_sm3.hlsl")
                                };
                                let vertex = if mat.techset.ends_with("_sco") {
                                    "lp_s_tc0n0_dtex_sm3_sco.hlsl"
                                } else {
                                    "lp_s_tc0n0_dtex_sm3.hlsl"
                                };
                                match self.program(vertex, &name) {
                                    Ok(value) => program = value,
                                    Err(error) => next.warnings.push(format!(
                                        "Model{id}: direct primary contribution omitted: {error}"
                                    )),
                                }
                            }
                        }
                        if volume.is_none() && mat.techset.starts_with("l_sm_") {
                            next.warnings.push(format!(
                                "Model{id} has no directional volume; lit surface omitted"
                            ));
                            continue;
                        }
                        let shadow_program = if direct
                            && mat.techset.starts_with("l_sm_")
                            && light.as_ref().is_some_and(shadows::eligible)
                        {
                            let kind = if light.as_ref().unwrap().r#type == 2 {
                                "spot"
                            } else {
                                "omni"
                            };
                            let stem = mat
                                .techset
                                .strip_prefix("l_sm_")
                                .unwrap_or("")
                                .split("_clothcharr")
                                .next()
                                .unwrap_or("")
                                .split("_sco")
                                .next()
                                .unwrap_or("");
                            match self.program(
                                "lp_hsm_omni_s_tc0n0_dtex_sm3.hlsl",
                                &format!("lp_hsm_{kind}_{stem}_sm3.hlsl"),
                            ) {
                                Ok(value) => Some(value),
                                Err(error) => {
                                    next.warnings.push(format!(
                                        "Model {id} shadow receiver omitted: {error}"
                                    ));
                                    None
                                }
                            }
                        } else {
                            None
                        };
                        next.draws.push(Draw {
                            mesh: *mesh,
                            first: 0,
                            count: next.meshes[*mesh].index_count,
                            material: mat.clone(),
                            program: program.clone(),
                            shadow_program,
                            kind: Kind::Model,
                            object: Some(id.clone()),
                            origin,
                            axis,
                            scale,
                            lightmap: 0,
                            light: light.clone(),
                            probe,
                            volume: volume.clone(),
                            scroll: [0.; 2],
                            sky_material: None,
                        });
                    }
                    next.model_instances += 1;
                }
            }
            if let Some(sky) = &m.sky {
                for value in sky["textures"].as_array().ok_or("Invalid sky textures")? {
                    let meta: Texture =
                        serde_json::from_value(value.clone()).map_err(|e| e.to_string())?;
                    next.textures.insert(
                        meta.id.clone(),
                        textures::upload(&self.device, &meta, &baked.buffers)?,
                    );
                }
                for layer in sky["layers"].as_array().ok_or("Invalid sky layers")? {
                    let sky_material = SkyMaterial::from_layer(layer)?;
                    let mat = Material {
                        name: layer["material"].as_str().unwrap_or("sky").into(),
                        techset: layer["techset"].as_str().unwrap_or("").into(),
                        textures: serde_json::from_value(layer["textures"].clone())
                            .map_err(|e| e.to_string())?,
                        env_map_parms: [0.; 4],
                        detail_scale: None,
                        color_tint: None,
                        states: serde_json::from_value(layer["states"].clone())
                            .map_err(|e| e.to_string())?,
                    };
                    let program = match self
                        .program(sky_material.vertex_program, sky_material.pixel_program)
                    {
                        Ok(p) => p,
                        Err(e) => {
                            next.warnings
                                .push(format!("Sky layer{} omitted:{e}", mat.name));
                            continue;
                        }
                    };
                    let verts = sky_vertices(buffer(
                        baked,
                        layer["vertexBuffer"]
                            .as_str()
                            .ok_or("Sky vertices missing")?,
                    )?)?;
                    let idx = buffer(
                        baked,
                        layer["indexBuffer"].as_str().ok_or("Sky indices missing")?,
                    )?
                    .chunks_exact(2)
                    .flat_map(|p| (u16::from_le_bytes(p.try_into().unwrap()) as u32).to_le_bytes())
                    .collect::<Vec<_>>();
                    let mesh = next.meshes.len();
                    next.meshes.push(self.mesh(&verts, &idx)?);
                    next.draws.push(Draw {
                        mesh,
                        first: 0,
                        count: next.meshes[mesh].index_count,
                        material: mat,
                        program,
                        shadow_program: None,
                        kind: Kind::Sky,
                        object: None,
                        origin: [0.; 3],
                        axis: [[1., 0., 0.], [0., 1., 0.], [0., 0., 1.]],
                        scale: 1.,
                        lightmap: 0,
                        light: None,
                        probe: 0,
                        volume: None,
                        scroll: vector(&layer["scroll"], [0.; 2]),
                        sky_material: Some(sky_material),
                    });
                }
            }
            // Final native admission happens after shader/model omissions.
            // Sky alone must not turn an otherwise empty candidate into success.
            if !next
                .draws
                .iter()
                .any(|draw| matches!(draw.kind, Kind::World | Kind::Model) && draw.count >= 3)
            {
                return Err("No supported native world or placed-model draws remain".into());
            }
            self.dirty = true;
            self.scene = Some(next);
            self.hidden.clear();
            self.last_error = None;
            Ok(())
        }
    }
    pub fn set_probe_cubes(&mut self, value: &Value) -> Result<()> {
        self.dirty = true;
        let values = value
            .as_array()
            .or_else(|| value["cubes"].as_array())
            .ok_or("Expected native probe cube list")?;
        let mut cubes = Vec::new();
        unsafe {
            for cube in values {
                cubes.push(textures::cube(&self.device, cube)?);
            }
        }
        self.scene
            .as_mut()
            .ok_or("No scene to receive probes")?
            .cubes = cubes;
        Ok(())
    }
    fn render_size(&self) -> Result<(u32, u32)> {
        let width = self.view["renderSize"]["width"]
            .as_u64()
            .unwrap_or(self.width as u64) as u32;
        let height = self.view["renderSize"]["height"]
            .as_u64()
            .unwrap_or(self.height as u64) as u32;
        if width == 0
            || height == 0
            || width > 8192
            || height > 8192
            || width as u64 * height as u64 > 8294400
        {
            return Err("Invalid native render resolution".into());
        }
        Ok((width, height))
    }
    fn target_size(&mut self, width: u32, height: u32) -> Result<()> {
        if self
            .frame
            .as_ref()
            .is_some_and(|f| f.width == width && f.height == height)
        {
            return Ok(());
        }
        unsafe {
            self.frame = Some(Frame::new(&self.device, width, height)?);
            self.output = Some(Frame::new(&self.device, width, height)?);
        }
        self.post.resize(width, height)?;
        Ok(())
    }
    fn local_light(&self) -> Option<&Light> {
        self.scene
            .as_ref()?
            .lights
            .iter()
            .find(|light| matches!(light.r#type, 2 | 3))
    }
    fn update_shadows(&mut self) -> Result<()> {
        let local = self.local_light();
        let Some(shadow) = &self.shadow else {
            self.shadow_matrices = None;
            return Ok(());
        };
        let matrices = shadow.render(&self.device, local, |device, pass| {
            let Some(scene) = &self.scene else {
                return Ok(());
            };
            unsafe {
                device
                    .SetVertexDeclaration(&self.declaration)
                    .map_err(err)?;
                for draw in &scene.draws {
                    if draw.kind == Kind::Sky
                        || draw
                            .object
                            .as_ref()
                            .is_some_and(|id| self.hidden.contains(id))
                    {
                        continue;
                    }
                    let word = draw.material.states[0];
                    if draw.kind == Kind::Model && (word >> 8) & 7 != 0 {
                        continue;
                    }
                    let alpha = if word & 2048 != 0 {
                        0
                    } else {
                        ((word >> 12) & 3) as u8
                    };
                    if draw.kind == Kind::Model && alpha != 0 {
                        if !pass.has_alpha_caster() {
                            continue;
                        }
                        let binding = draw
                            .material
                            .textures
                            .iter()
                            .find(|v| v.semantic == "colorMap");
                        let image = binding
                            .and_then(|v| scene.textures.get(&v.texture_id))
                            .unwrap_or(&self.white);
                        pass.bind_alpha_object(
                            device,
                            draw.axis,
                            draw.origin,
                            draw.scale,
                            image,
                            alpha,
                        )?;
                    } else {
                        pass.bind_object(device, draw.axis, draw.origin, draw.scale)?;
                    }
                    // The world profile is opaque. Model cutouts keep material culling.
                    let cull = if draw.kind == Kind::Model {
                        match (word >> 14) & 3 {
                            2 => {
                                if draw.scale < 0.0 {
                                    2
                                } else {
                                    3
                                }
                            }
                            3 => {
                                if draw.scale < 0.0 {
                                    3
                                } else {
                                    2
                                }
                            }
                            _ => 1,
                        }
                    } else {
                        1
                    };
                    device.SetRenderState(D3DRS_CULLMODE, cull).map_err(err)?;
                    let mesh = &scene.meshes[draw.mesh];
                    device
                        .SetStreamSource(0, &mesh.vertices, 0, 44)
                        .map_err(err)?;
                    device.SetIndices(&mesh.indices).map_err(err)?;
                    device
                        .DrawIndexedPrimitive(
                            D3DPT_TRIANGLELIST,
                            0,
                            0,
                            mesh.vertex_count,
                            draw.first,
                            draw.count / 3,
                        )
                        .map_err(err)?;
                }
            }
            Ok(())
        })?;
        self.shadow_matrices = matrices;
        Ok(())
    }
    pub fn render(&mut self) -> Result<()> {
        let (width, height) = self.render_size()?;
        let camera = self.effect_camera(width, height);
        let elapsed = self.fx_tick.elapsed().as_secs_f64().min(0.1);
        self.fx_tick = Instant::now();
        if let Some(effect) = self.fx.as_mut() {
            match effect.update(&camera, elapsed) {
                Ok(changed) => self.dirty |= changed,
                Err(error) => self.omit_fx(error)?,
            }
        }
        if !self.dirty {
            return Ok(());
        }
        self.target_size(width, height)?;
        self.update_shadows()?;
        let frame = self.frame.as_ref().unwrap();
        let eye = vector(&self.view["camera"]["position"], [0.; 3]);
        let matrix = vector(
            &self.view["viewProjection"],
            camera_matrix(&self.view["camera"], width as f32 / height as f32),
        );
        unsafe {
            self.draw_scene(frame, eye, matrix, false)?;
        }
        if let Err(error) = self.draw_effects(&camera, eye, matrix) {
            self.omit_fx(error)?;
            // A failed effect may already have touched colour/depth. Rebuild
            // the backdrop before presenting so partial FX cannot remain.
            let frame = self.frame.as_ref().unwrap();
            unsafe {
                self.draw_scene(frame, eye, matrix, false)?;
            }
        }
        let frame = self.frame.as_ref().unwrap();
        unsafe {
            let output = self.output.as_ref().unwrap();
            self.post
                .process(&frame.texture, &output.surface, &self.view)?;
            if let Some(chain) = &self.swapchain {
                let back = chain
                    .GetBackBuffer(0, D3DBACKBUFFER_TYPE_MONO)
                    .map_err(err)?;
                self.device
                    .StretchRect(&output.surface, null(), &back, null(), D3DTEXF_LINEAR)
                    .map_err(err)?;
                chain
                    .Present(null(), null(), HWND::default(), null(), 0)
                    .map_err(err)?;
            }
        }
        self.dirty = false;
        Ok(())
    }
    fn draw_effects(&mut self, camera: &FxCamera, eye: [f32; 3], matrix: [f32; 16]) -> Result<()> {
        let Some(fx) = self.fx.as_mut() else {
            return Ok(());
        };
        let frame = self.frame.as_ref().ok_or("No scene target for effects")?;
        let float_z = self
            .float_z
            .as_mut()
            .ok_or("No linear-depth pass for effects")?;
        float_z.resize(&self.device, frame.width, frame.height)?;
        let fog_enabled = self.view["fogEnabled"]
            .as_bool()
            .or_else(|| self.view["fog"]["enabled"].as_bool())
            .unwrap_or(false);
        let fog = if fog_enabled {
            vector(&self.view["fogConsts"], [0.0; 4])
        } else {
            [0.0; 4]
        };
        let fog_color = if fog_enabled {
            vector(&self.view["fogColor"], [0.0; 4])
        } else {
            [0.0; 4]
        };
        fx.set_environment(camera, float_z.texture()?, fog, fog_color)?;
        unsafe {
            let saved = self.device.CreateStateBlock(D3DSBT_ALL).map_err(err)?;
            // FX material stencil tests expect a known empty stencil. Retain
            // the scene colour and hardware depth used for occlusion.
            self.device
                .Clear(0, null(), D3DCLEAR_STENCIL as u32, 0, 1.0, 0)
                .map_err(err)?;
            self.device.BeginScene().map_err(err)?;
            let drawn = fx.draw_models(camera);
            let end = self.device.EndScene().map_err(err);
            let restored = saved.Apply().map_err(err);
            drawn.and(end).and(restored)?;
        }
        float_z.render(
            &self.device,
            &frame.depth,
            &transpose(matrix),
            eye,
            |device, pass| {
                unsafe {
                    device
                        .SetVertexDeclaration(&self.declaration)
                        .map_err(err)?;
                    if let Some(scene) = &self.scene {
                        for draw in &scene.draws {
                            let word = draw.material.states[0];
                            let blend = (word >> 8) & 7;
                            if draw.kind == Kind::Sky
                                || draw.material.techset == "objective"
                                || blend != 0 && draw.material.states[1] & 1 == 0
                                || draw
                                    .object
                                    .as_ref()
                                    .is_some_and(|id| self.hidden.contains(id))
                            {
                                continue;
                            }
                            pass.bind_object(draw.axis, draw.origin, draw.scale)?;
                            let mesh = &scene.meshes[draw.mesh];
                            device
                                .SetStreamSource(0, &mesh.vertices, 0, 44)
                                .map_err(err)?;
                            device.SetIndices(&mesh.indices).map_err(err)?;
                            device.SetRenderState(D3DRS_CULLMODE, 1).map_err(err)?;
                            device
                                .DrawIndexedPrimitive(
                                    D3DPT_TRIANGLELIST,
                                    0,
                                    0,
                                    mesh.vertex_count,
                                    draw.first,
                                    draw.count / 3,
                                )
                                .map_err(err)?;
                        }
                    }
                }
                fx.draw_float_z(pass)
            },
        )?;
        // Refraction samples the scene before particles are drawn.
        fx.copy_pre_emissive(&frame.surface, frame.width, frame.height)?;
        unsafe {
            let saved = self.device.CreateStateBlock(D3DSBT_ALL).map_err(err)?;
            self.device
                .SetRenderTarget(0, &frame.surface)
                .map_err(err)?;
            self.device
                .SetDepthStencilSurface(&frame.depth)
                .map_err(err)?;
            self.device
                .SetViewport(&D3DVIEWPORT9 {
                    X: 0,
                    Y: 0,
                    Width: frame.width,
                    Height: frame.height,
                    MinZ: 0.0,
                    MaxZ: 1.0,
                })
                .map_err(err)?;
            self.device.BeginScene().map_err(err)?;
            let drawn = fx.draw_particles(camera, float_z.texture()?, fog, fog_color);
            let end = self.device.EndScene().map_err(err);
            let restored = saved.Apply().map_err(err);
            drawn.and(end).and(restored)?;
        }
        Ok(())
    }
    unsafe fn constant(
        &self,
        program: &Program,
        vertex: bool,
        name: &str,
        values: &[f32],
    ) -> Result<()> {
        let metadata = if vertex {
            &program.vertex_metadata
        } else {
            &program.pixel_metadata
        };
        if let Some(c) = metadata.constants.iter().find(|c| c.name == name) {
            if values.len() % 4 != 0 || values.len() / 4 > c.count as usize {
                return Err(format!("Invalid constant{name}"));
            }
            if vertex {
                self.device
                    .SetVertexShaderConstantF(
                        c.register,
                        values.as_ptr(),
                        (values.len() / 4) as u32,
                    )
                    .map_err(err)?;
            } else {
                self.device
                    .SetPixelShaderConstantF(c.register, values.as_ptr(), (values.len() / 4) as u32)
                    .map_err(err)?;
            }
        }
        Ok(())
    }
    unsafe fn sampler(&self, index: u32, flags: u32, clamp: bool) -> Result<()> {
        let value = flags & 255;
        let filter = if value & 31 == 1 { 1 } else { 2 };
        for (state, v) in [
            (
                D3DSAMP_ADDRESSU,
                if clamp || value & 32 != 0 { 3 } else { 1 },
            ),
            (
                D3DSAMP_ADDRESSV,
                if clamp || value & 64 != 0 { 3 } else { 1 },
            ),
            (
                D3DSAMP_ADDRESSW,
                if clamp || value & 128 != 0 { 3 } else { 1 },
            ),
            (
                D3DSAMP_MINFILTER,
                if filter == 2 && !clamp { 3 } else { filter },
            ),
            (D3DSAMP_MAGFILTER, filter),
            (D3DSAMP_MIPFILTER, if value & 31 >= 10 { 1 } else { 0 }),
            (D3DSAMP_MAXANISOTROPY, 2),
            (D3DSAMP_SRGBTEXTURE, 0),
        ] {
            self.device.SetSamplerState(index, state, v).map_err(err)?;
        }
        Ok(())
    }
    unsafe fn texture(
        &self,
        p: &Program,
        name: &str,
        texture: &IDirect3DTexture9,
        flags: u32,
        clamp: bool,
    ) -> Result<()> {
        if let Some(c) = p.pixel_metadata.constants.iter().find(|c| c.name == name) {
            self.device.SetTexture(c.register, texture).map_err(err)?;
            self.sampler(c.register, flags, clamp)?;
        }
        Ok(())
    }
    unsafe fn draw_scene(
        &self,
        frame: &Frame,
        eye: [f32; 3],
        matrix: [f32; 16],
        capture: bool,
    ) -> Result<()> {
        let d = &self.device;
        for unit in 0..16 {
            d.SetTexture(unit, None::<&IDirect3DBaseTexture9>)
                .map_err(err)?;
        }
        d.SetRenderTarget(0, &frame.surface).map_err(err)?;
        d.SetDepthStencilSurface(&frame.depth).map_err(err)?;
        d.SetViewport(&D3DVIEWPORT9 {
            X: 0,
            Y: 0,
            Width: frame.width,
            Height: frame.height,
            MinZ: 0.,
            MaxZ: 1.,
        })
        .map_err(err)?;
        d.Clear(
            0,
            null(),
            D3DCLEAR_TARGET as u32 | D3DCLEAR_ZBUFFER as u32,
            0xff000000,
            1.,
            0,
        )
        .map_err(err)?;
        d.BeginScene().map_err(err)?;
        let result = (|| -> Result<()> {
            let Some(scene) = &self.scene else {
                return Ok(());
            };
            d.SetVertexDeclaration(&self.declaration).map_err(err)?;
            let vp = transpose(matrix);
            let clock = if capture {
                PreviewClock::ZERO
            } else {
                PreviewClock::from_view(&self.view)?
            };
            let local = self.local_light();
            let fog_enabled = !capture
                && self.view["fogEnabled"]
                    .as_bool()
                    .or_else(|| self.view["fog"]["enabled"].as_bool())
                    .unwrap_or(false);
            let fog = if fog_enabled {
                vector(
                    &self.view["fogConsts"],
                    vector(&self.view["fog"]["fogConsts"], [0.; 4]),
                )
            } else {
                [0.; 4]
            };
            let fog_color = if fog_enabled {
                vector(
                    &self.view["fogColor"],
                    vector(&self.view["fog"]["fogColor"], [0.; 4]),
                )
            } else {
                [0.; 4]
            };
            let mut order = scene
                .draws
                .iter()
                .filter(|x| x.object.as_ref().is_none_or(|id| !self.hidden.contains(id)))
                .collect::<Vec<_>>();
            order.sort_by(|a, b| {
                if a.kind == Kind::Sky && b.kind == Kind::Sky {
                    return a
                        .sky_material
                        .map(|v| v.sort)
                        .cmp(&b.sky_material.map(|v| v.sort));
                }
                let ba = ((a.material.states[0] >> 8) & 7) != 0;
                let bb = ((b.material.states[0] >> 8) & 7) != 0;
                ba.cmp(&bb).then_with(|| {
                    if ba {
                        let distance = |v: &Draw| {
                            v.origin
                                .iter()
                                .zip(eye)
                                .map(|(x, y)| (x - y) * (x - y))
                                .sum::<f32>()
                        };
                        distance(b).total_cmp(&distance(a))
                    } else {
                        std::cmp::Ordering::Equal
                    }
                })
            });
            for draw in order {
                let light = draw.light.as_ref();
                let use_shadow = !capture
                    && self.shadow_matrices.is_some()
                    && local
                        .as_ref()
                        .zip(light)
                        .is_some_and(|(local, light)| local.index == light.index)
                    && draw.shadow_program.is_some();
                let p = if use_shadow {
                    draw.shadow_program.as_ref().unwrap()
                } else {
                    &draw.program
                };
                let mesh = &scene.meshes[draw.mesh];
                d.SetVertexShader(&p.vertex).map_err(err)?;
                d.SetPixelShader(&p.pixel).map_err(err)?;
                d.SetStreamSource(0, &mesh.vertices, 0, 44).map_err(err)?;
                d.SetIndices(&mesh.indices).map_err(err)?;
                let sky = draw.kind == Kind::Sky;
                let mut world = [0.; 16];
                for row in 0..3 {
                    for col in 0..3 {
                        world[row * 4 + col] = draw.axis[col][row] * draw.scale;
                    }
                    world[row * 4 + 3] = if sky {
                        0.0
                    } else {
                        draw.origin[row] - eye[row]
                    };
                }
                world[15] = if sky { 0.0 } else { 1.0 };
                self.constant(p, true, "viewProjectionMatrix", &vp)?;
                self.constant(p, true, "worldViewProjectionMatrix", &vp)?;
                self.constant(p, true, "worldMatrix", &world)?;
                let yaw = number(&self.view["camera"]["yaw"], 0.0);
                let pitch = number(&self.view["camera"]["pitch"], 0.0);
                let view_axes = [
                    [yaw.sin(), -yaw.cos(), 0.0],
                    [
                        -pitch.sin() * yaw.cos(),
                        -pitch.sin() * yaw.sin(),
                        pitch.cos(),
                    ],
                    [
                        pitch.cos() * yaw.cos(),
                        pitch.cos() * yaw.sin(),
                        pitch.sin(),
                    ],
                ];
                let mut normal_view = [0.0; 12];
                for row in 0..3 {
                    for col in 0..3 {
                        normal_view[row * 4 + col] =
                            (0..3).map(|i| view_axes[row][i] * draw.axis[col][i]).sum();
                    }
                }
                self.constant(p, true, "inverseTransposeWorldViewMatrix", &normal_view)?;

                self.constant(p, true, "fogConsts", &fog)?;
                self.constant(p, true, "baseLightingCoords", &[0.5, 0.5, 0.5, 0.])?;
                self.constant(p, true, "destructibleParms", &[0.; 4])?;
                self.constant(p, true, "gameTime", &clock.game_time())?;
                self.constant(
                    p,
                    true,
                    "viewMatrix",
                    &[
                        1., 0., 0., 0., 0., 1., 0., 0., 0., 0., 1., 0., 0., 0., 0., 1.,
                    ],
                )?;
                self.constant(
                    p,
                    true,
                    "uvScroll",
                    &[draw.scroll[0], draw.scroll[1], 0., 0.],
                )?;
                self.constant(p, false, "fogColor", &fog_color)?;
                self.constant(p, false, "envMapParms", &draw.material.env_map_parms)?;
                self.constant(
                    p,
                    false,
                    "detailScale",
                    &draw.material.detail_scale.unwrap_or([1., 1., 0., 0.]),
                )?;
                self.constant(
                    p,
                    false,
                    "colorTint",
                    &draw.material.color_tint.unwrap_or([1.; 4]),
                )?;
                self.constant(p, false, "lightingLookupScale", &[0.375, 0.375, 0.375, 0.])?;
                self.constant(p, false, "gameTime", &clock.game_time())?;
                if let Some(sky_material) = draw.sky_material {
                    let values = sky_material.constants(clock, fog_enabled, fog, fog_color);
                    self.constant(p, true, "fogConsts", &values.fog_consts)?;
                    self.constant(p, false, "fogColor", &values.fog_color)?;
                    self.constant(p, true, "uvScroll", &values.uv_scroll)?;
                    self.constant(p, false, "skyTransition", &values.sky_transition)?;
                }
                for (semantic, name, fallback) in [
                    ("colorMap", "colorMapSampler", &self.white),
                    ("normalMap", "normalMapSampler", &self.normal),
                    ("specularMap", "specularMapSampler", &self.black),
                    ("detailMap", "detailMapSampler", &self.normal),
                    ("colorMap01", "colorMapSampler1", &self.white),
                    ("colorMap01", "colorMapSampler2", &self.white),
                ] {
                    let binding = draw
                        .material
                        .textures
                        .iter()
                        .find(|b| b.semantic == semantic);
                    let image = binding
                        .and_then(|b| scene.textures.get(&b.texture_id))
                        .unwrap_or(fallback);
                    self.texture(
                        p,
                        name,
                        image,
                        binding.map(|b| b.flags).unwrap_or(0x20b),
                        false,
                    )?;
                }
                if let Some(map) = scene.lightmaps.get(draw.lightmap) {
                    self.texture(p, "lightmapSamplerPrimary", &map.0, 0x62, true)?;
                    self.texture(p, "lightmapSamplerSecondary", &map.1, 0x62, true)?;
                }
                if let Some(c) = p
                    .pixel_metadata
                    .constants
                    .iter()
                    .find(|c| c.name == "modelLightingSampler")
                {
                    if let Some(volume) = &draw.volume {
                        d.SetTexture(c.register, volume).map_err(err)?;
                        self.sampler(c.register, 0xe2, true)?;
                    }
                }
                if let Some(c) = p
                    .pixel_metadata
                    .constants
                    .iter()
                    .find(|c| c.name == "reflectionProbeSampler")
                {
                    let enabled =
                        !capture && self.view["reflectionsEnabled"].as_bool().unwrap_or(true);
                    let cube = if enabled {
                        scene
                            .cubes
                            .get(draw.probe.wrapping_sub(1))
                            .unwrap_or(&self.black_cube)
                    } else {
                        &self.black_cube
                    };
                    d.SetTexture(c.register, cube).map_err(err)?;
                    self.sampler(c.register, 0xe2, true)?;
                    d.SetSamplerState(c.register, D3DSAMP_MIPFILTER, 2)
                        .map_err(err)?;
                }
                if let Some(light) = light {
                    let color = [light.rgb[0], light.rgb[1], light.rgb[2], 0.];
                    let spec = [light.rgb[0], light.rgb[1], light.rgb[2], 0.];
                    for name in ["sunPosition", "lightSpotDir"] {
                        self.constant(
                            p,
                            false,
                            name,
                            &[
                                light.direction[0],
                                light.direction[1],
                                light.direction[2],
                                0.,
                            ],
                        )?;
                    }
                    self.constant(
                        p,
                        false,
                        "lightPosition",
                        &[
                            light.origin[0] - eye[0],
                            light.origin[1] - eye[1],
                            light.origin[2] - eye[2],
                            1. / light.radius.max(0.001),
                        ],
                    )?;
                    for name in ["sunDiffuse", "lightDiffuse"] {
                        self.constant(p, false, name, &color)?;
                    }
                    for name in ["sunSpecular", "lightSpecular"] {
                        self.constant(p, false, name, &spec)?;
                    }
                    let diff = (light.cos_inner - light.cos_outer).max(0.000001);
                    self.constant(
                        p,
                        false,
                        "lightSpotFactors",
                        &[
                            1. / diff,
                            -light.cos_outer / diff,
                            light.exponent as f32,
                            if use_shadow {
                                number(&self.view["shadowFade"], 1.0).clamp(0.0, 1.0)
                            } else {
                                0.0
                            },
                        ],
                    )?;
                    self.constant(
                        p,
                        false,
                        "lightFalloffPlacement",
                        scene.falloffs.get(&light.def_name).unwrap_or(&[0.; 4]),
                    )?;
                }
                if use_shadow {
                    let lookup = self.shadow_matrices.as_ref().unwrap().lookup_rows(eye);
                    self.constant(p, true, "shadowLookupMatrix", &lookup)?;
                    self.constant(p, false, "spotShadowmapPixelAdjust", &shadows::PIXEL_ADJUST)?;
                    for name in ["shadowmapSamplerSpot", "shadowmapSampler"] {
                        if let Some(binding) = p.pixel_metadata.constant(name) {
                            self.shadow.as_ref().unwrap().bind(d, binding.register)?;
                        }
                    }
                }
                let word = draw.material.states[0];
                let blend = (word >> 8) & 7;
                let objective = draw.material.techset == "objective";
                let premult = draw.kind == Kind::Model && draw.material.techset.contains("_b0");
                let cull = if sky { 0 } else { (word >> 14) & 3 };
                let cull = match cull {
                    2 => {
                        if draw.scale < 0. {
                            2
                        } else {
                            3
                        }
                    }
                    3 => {
                        if draw.scale < 0. {
                            3
                        } else {
                            2
                        }
                    }
                    _ => 1,
                };
                let alpha = if word & 2048 != 0 {
                    0
                } else {
                    (word >> 12) & 3
                };
                let src = if objective {
                    5
                } else if premult {
                    2
                } else {
                    word & 15
                };
                let dst = if objective {
                    2
                } else if premult {
                    6
                } else {
                    (word >> 4) & 15
                };
                for (state, value) in [
                    (D3DRS_ZENABLE, 1),
                    (
                        D3DRS_ZWRITEENABLE,
                        u32::from(
                            !sky && !objective && (blend == 0 || draw.material.states[1] & 1 != 0),
                        ),
                    ),
                    (D3DRS_ZFUNC, 4),
                    (D3DRS_CULLMODE, cull),
                    (D3DRS_ALPHABLENDENABLE, u32::from(blend != 0 || objective)),
                    (D3DRS_SRCBLEND, src.max(1)),
                    (D3DRS_DESTBLEND, dst.max(1)),
                    (D3DRS_BLENDOP, if blend == 0 { 1 } else { blend.min(3) }),
                    (D3DRS_SEPARATEALPHABLENDENABLE, 1),
                    (D3DRS_SRCBLENDALPHA, 8),
                    (D3DRS_DESTBLENDALPHA, 2),
                    (D3DRS_BLENDOPALPHA, 1),
                    (D3DRS_ALPHATESTENABLE, u32::from(alpha != 0)),
                    (D3DRS_ALPHAREF, if alpha == 3 { 128 } else { 0 }),
                    (D3DRS_ALPHAFUNC, if alpha == 2 { 2 } else { 5 }),
                    (D3DRS_SRGBWRITEENABLE, 0),
                    (D3DRS_FOGENABLE, 0),
                    (D3DRS_COLORWRITEENABLE, 15),
                ] {
                    d.SetRenderState(state, value).map_err(err)?;
                }
                if draw.first + draw.count > mesh.index_count {
                    return Err("Draw exceeds native index buffer".into());
                }
                d.DrawIndexedPrimitive(
                    D3DPT_TRIANGLELIST,
                    0,
                    0,
                    mesh.vertex_count,
                    draw.first,
                    draw.count / 3,
                )
                .map_err(err)?;
            }
            Ok(())
        })();
        let end = d.EndScene().map_err(err);
        result.and(end)
    }
    pub fn readback(&mut self) -> Result<(u32, u32, Vec<u8>)> {
        let frame = self.output.as_ref().ok_or("No rendered scene")?;
        unsafe { self.read_surface(&frame.surface, frame.width, frame.height) }
    }
    unsafe fn read_surface(
        &self,
        surface: &IDirect3DSurface9,
        width: u32,
        height: u32,
    ) -> Result<(u32, u32, Vec<u8>)> {
        let mut output = None;
        self.device
            .CreateOffscreenPlainSurface(
                width,
                height,
                D3DFMT_A8R8G8B8,
                D3DPOOL_SYSTEMMEM,
                &mut output,
                null_mut(),
            )
            .map_err(err)?;
        let output = output.ok_or("No readback surface")?;
        self.device
            .GetRenderTargetData(surface, &output)
            .map_err(err)?;
        let mut lock = D3DLOCKED_RECT::default();
        output
            .LockRect(&mut lock, null(), D3DLOCK_READONLY as u32)
            .map_err(err)?;
        let mut result = vec![0; width as usize * height as usize * 4];
        for y in 0..height as usize {
            let row = std::slice::from_raw_parts(
                lock.pBits.cast::<u8>().add(y * lock.Pitch as usize),
                width as usize * 4,
            );
            for (x, p) in row.chunks_exact(4).enumerate() {
                result[(y * width as usize + x) * 4..][..4]
                    .copy_from_slice(&[p[2], p[1], p[0], p[3]]);
            }
        }
        output.UnlockRect().map_err(err)?;
        Ok((width, height, result))
    }
    pub fn capture_face(&mut self, origin: [f32; 3], matrix: [f32; 16]) -> Result<Vec<u8>> {
        unsafe {
            let frame = Frame::new(&self.device, 258, 258)?;
            self.draw_scene(&frame, origin, matrix, true)?;
            Ok(self.read_surface(&frame.surface, 258, 258)?.2)
        }
    }
    pub fn diagnostics(&self) -> Value {
        let mut counts = serde_json::Map::new();
        let mut model_count = 0;
        let mut triangles = 0;
        let mut warnings = Vec::new();
        if let Some(scene) = &self.scene {
            for draw in &scene.draws {
                if let Some(id) = &draw.object {
                    let current = counts.get(id).and_then(Value::as_u64).unwrap_or(0);
                    counts.insert(
                        id.clone(),
                        json!(current + u64::from(!self.hidden.contains(id))),
                    );
                }
                if draw.kind == Kind::Model {
                    model_count += 1;
                }
                triangles += draw.count / 3;
            }
            warnings = scene.warnings.clone();
        }
        if let Some(warning) = &self.shadow_warning {
            warnings.push(warning.clone());
        }
        if let Some(warning) = self.shadow.as_ref().and_then(|v| v.alpha_warning.as_ref()) {
            warnings.push(warning.clone());
        }
        let scene = self.scene.as_ref();
        let frame = self.frame.as_ref();
        let effects: Vec<_> = self.fx.iter().map(NativeSceneFx::diagnostics).collect();
        json!({
            "fx": effects.first(),
            "fxEntries": effects,
            "fxCount": effects.len(),
            "fxWarning": self.fx_warning,
            "floatZ": self.float_z.as_ref().map(FloatZ::diagnostics),
            "backend": "Direct3D9",
            "adapter": self.adapter.metadata(),
            "renderOnDemand": true,
            "originalShaders": true,
            "sceneId": scene.map(|scene| &scene.source_id),
            "renderSize": {
                "width": frame.map_or(self.width, |frame| frame.width),
                "height": frame.map_or(self.height, |frame| frame.height)
            },
            "models": {
                "instances": scene.map_or(0, |scene| scene.model_instances),
                "assets": scene.map_or(0, |scene| scene.model_assets),
                "surfaces": model_count,
                "objectDrawCounts": counts,
                "warnings": warnings
            },
            "triangles": triangles,
            "warnings": warnings,
            "lastError": self.last_error,
            "camera": self.view["camera"],
            "worldspawnPreview": self.view["worldspawn"],
            "dynamicShadow": {
                "available": self.shadow.is_some(),
                "active": self.shadow_matrices.is_some(),
                "format": "D24S8 / LINEAR comparison / 512x2048",
                "colorTarget": self.shadow.as_ref().map(|shadow| shadow.color_target_format)
            }
        })
    }
}
fn transpose(a: [f32; 16]) -> [f32; 16] {
    let mut out = [0.; 16];
    for i in 0..4 {
        for j in 0..4 {
            out[i * 4 + j] = a[j * 4 + i];
        }
    }
    out
}
fn camera_matrix(camera: &Value, aspect: f32) -> [f32; 16] {
    let yaw = number(&camera["yaw"], 0.);
    let pitch = number(&camera["pitch"], 0.);
    let fov = number(&camera["fov"], 51.077194);
    let f = [
        pitch.cos() * yaw.cos(),
        pitch.cos() * yaw.sin(),
        pitch.sin(),
    ];
    let r = [yaw.sin(), -yaw.cos(), 0.];
    let u = [
        -pitch.sin() * yaw.cos(),
        -pitch.sin() * yaw.sin(),
        pitch.cos(),
    ];
    let a = 2047. / 2048.;
    let focal = a / (fov.to_radians() * 0.5).tan();
    let mut out = [0.; 16];
    for i in 0..3 {
        out[i * 4] = r[i] * focal / aspect;
        out[i * 4 + 1] = u[i] * focal;
        out[i * 4 + 2] = f[i] * a;
        out[i * 4 + 3] = f[i];
    }
    out[14] = -4. * a;
    out
}
