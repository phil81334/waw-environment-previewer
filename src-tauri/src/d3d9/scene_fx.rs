//! Supported EFX playback inside the existing compiled-scene D3D9 device.
//!
//! This module owns neither a window nor the parent's scene targets/clock.
//! Only a two-packet CPU queue and the selected effect's resources are retained.
use crate::{fx, scene_fx::PreparedEffect};
use serde_json::{json, Value};
use std::{
    path::Path,
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver, TryRecvError},
        Arc,
    },
};
use windows::Win32::Graphics::Direct3D9::*;

type Result<T> = std::result::Result<T, String>;
fn error(value: impl std::fmt::Display) -> String {
    value.to_string()
}

#[derive(Clone, Debug, PartialEq)]
pub struct FxCamera {
    pub origin: [f32; 3],
    /// Native forward / left / up axes.
    pub axis: [[f32; 3]; 3],
    /// Absolute world-to-clip rows, not the parent's camera-relative rows.
    pub view_projection_rows: [f32; 16],
    /// Exact parent matrix, shared with hardware-depth and float-Z replay.
    pub relative_view_projection_rows: [f32; 16],
    pub view_rows: [f32; 16],
    pub projection_rows: [f32; 16],
    pub width: u32,
    pub height: u32,
}

impl FxCamera {
    fn validate(&self) -> Result<()> {
        if self.width == 0
            || self.height == 0
            || self.width > 8192
            || self.height > 8192
            || u64::from(self.width) * u64::from(self.height) > 8_294_400
            || !self
                .origin
                .iter()
                .chain(self.axis.iter().flatten())
                .chain(&self.view_projection_rows)
                .chain(&self.relative_view_projection_rows)
                .chain(&self.view_rows)
                .chain(&self.projection_rows)
                .all(|value| value.is_finite())
        {
            return Err("Invalid scene effect camera.".into());
        }
        fx::camera::parse_axis(Some(&json!(self.axis)))?;
        Ok(())
    }
}

struct Timeline {
    receiver: Receiver<Result<fx::simulation::StreamFrame>>,
    cancelled: Arc<AtomicBool>,
}

impl Timeline {
    fn start(prepared: &PreparedEffect) -> Self {
        let (sender, receiver) = mpsc::sync_channel(2);
        let cancelled = Arc::new(AtomicBool::new(false));
        let stop = cancelled.clone();
        let source = prepared.source.clone();
        let seed = prepared.seed;
        std::thread::spawn(move || {
            let mut emit = |packet: fx::simulation::StreamFrame| -> Result<bool> {
                if stop.load(Ordering::Acquire) {
                    return Ok(false);
                }
                Ok(sender.send(Ok(packet)).is_ok() && !stop.load(Ordering::Acquire))
            };
            if let Err(message) = fx::simulation::stream(&source, seed, 33, &mut emit) {
                if !stop.load(Ordering::Acquire) {
                    let _ = sender.send(Err(message));
                }
            }
        });
        Self {
            receiver,
            cancelled,
        }
    }
    fn next(&self) -> Result<Option<fx::simulation::StreamFrame>> {
        match self.receiver.try_recv() {
            Ok(packet) => packet.map(Some),
            Err(TryRecvError::Empty | TryRecvError::Disconnected) => Ok(None),
        }
    }
}
impl Drop for Timeline {
    fn drop(&mut self) {
        self.cancelled.store(true, Ordering::Release);
    }
}

#[derive(Clone, PartialEq)]
struct Controls {
    enabled: bool,
    playing: bool,
    repeat: bool,
    time_ms: f64,
    explicit_time: bool,
    restart_revision: u64,
}
impl Default for Controls {
    fn default() -> Self {
        Self {
            enabled: true,
            playing: false,
            repeat: false,
            time_ms: 0.,
            explicit_time: false,
            restart_revision: 0,
        }
    }
}

pub struct NativeSceneFx {
    renderer: fx::renderer::Renderer,
    prepared: Option<PreparedEffect>,
    projection: Option<fx::prepare::FrameProjection>,
    timeline: Option<Timeline>,
    current: Option<fx::simulation::StreamFrame>,
    pending: Option<fx::simulation::StreamFrame>,
    frame: Value,
    controls: Controls,
    last_camera: Option<FxCamera>,
    last_target: f64,
    completed_at: Option<u32>,
    duration_hint: Option<u32>,
    presented_time: Option<u32>,
    dirty: bool,
}

impl NativeSceneFx {
    pub fn new(device: &IDirect3DDevice9, game: &Path) -> Result<Self> {
        let renderer = unsafe { fx::renderer::Renderer::from_device(device, game).map_err(error)? };
        Ok(Self {
            renderer,
            prepared: None,
            projection: None,
            timeline: None,
            current: None,
            pending: None,
            frame: Value::Null,
            controls: Controls::default(),
            last_camera: None,
            last_target: 0.,
            completed_at: None,
            duration_hint: None,
            presented_time: None,
            dirty: true,
        })
    }

    pub fn load(&mut self, prepared: &PreparedEffect) -> Result<Value> {
        unsafe {
            self.renderer
                .load_frames(&prepared.materials)
                .map_err(error)?;
        }
        let projection = fx::prepare::FrameProjection::new(&prepared.source["modelInfo"])?;
        self.prepared = Some(prepared.clone());
        self.projection = Some(projection);
        self.duration_hint = None;
        self.restart();
        Ok(prepared.metadata())
    }

    fn restart(&mut self) {
        self.timeline = self.prepared.as_ref().map(Timeline::start);
        self.current = None;
        self.pending = None;
        self.frame = Value::Null;
        self.completed_at = None;
        self.presented_time = None;
        self.last_target = 0.;
        self.dirty = true;
    }

    pub fn set_controls(&mut self, value: &Value) -> Result<()> {
        if value.is_null() {
            return Ok(());
        }
        if !value.is_object() {
            return Err("Invalid effect preview controls.".into());
        }
        let mut next = self.controls.clone();
        for (field, destination) in [
            ("enabled", &mut next.enabled),
            ("playing", &mut next.playing),
            ("repeat", &mut next.repeat),
        ] {
            if let Some(input) = value.get(field) {
                *destination = input.as_bool().ok_or("Effect control must be a boolean.")?;
            }
        }
        if let Some(input) = value.get("timeMs") {
            next.time_ms = input
                .as_f64()
                .filter(|value| value.is_finite() && (0.0..=60000.0).contains(value))
                .ok_or("Effect preview time must be between 0 and 60000 milliseconds.")?;
            next.explicit_time = true;
        }
        if let Some(input) = value.get("restartRevision") {
            next.restart_revision = input.as_u64().ok_or("Invalid effect restart revision.")?;
        }
        if next != self.controls {
            let restart = next.restart_revision != self.controls.restart_revision;
            self.controls = next;
            if restart {
                self.restart();
            }
            self.dirty = true;
        }
        Ok(())
    }

    /// Explicit UI milliseconds win over delta time, preventing a second clock.
    /// Drawing while paused rebuilds from the same retained particle snapshot.
    pub fn update(&mut self, camera: &FxCamera, delta_seconds: f64) -> Result<bool> {
        camera.validate()?;
        if !self.controls.explicit_time && self.controls.playing && self.controls.enabled {
            self.controls.time_ms =
                (self.controls.time_ms + delta_seconds.clamp(0., 0.1) * 1000.).min(60000.);
        }
        let mut target = self.controls.time_ms;
        if self.controls.repeat {
            if let Some(end) = self.duration_hint.filter(|end| *end > 0) {
                target %= f64::from(end);
            }
        }
        if target < self.last_target {
            self.restart();
        }
        self.last_target = target;
        let mut changed = self.dirty || self.last_camera.as_ref() != Some(camera);
        // A large seek cannot monopolize the UI/render thread. The producer is
        // naturally backpressured and subsequent ticks continue toward target.
        for _ in 0..8 {
            let packet = if let Some(packet) = self.pending.take() {
                Some(packet)
            } else {
                self.timeline
                    .as_ref()
                    .map(Timeline::next)
                    .transpose()?
                    .flatten()
            };
            let Some(packet) = packet else {
                break;
            };
            let time = packet.frame["time"]
                .as_u64()
                .ok_or("Effect frame has no time.")?;
            if time as f64 > target {
                self.pending = Some(packet);
                break;
            }
            if packet.lifecycle.completion_time.is_some()
                && self.current.as_ref().is_some_and(|current| {
                    current.frame["sceneVisual"] == true
                        && current.frame["time"].as_u64().map(|time| time as u32)
                            != self.presented_time
                })
            {
                // Do not consume a real millisecond burst and its terminal blank
                // in one UI tick without ever presenting its visual sample.
                self.pending = Some(packet);
                break;
            }
            self.completed_at = packet.lifecycle.completion_time;
            if self.completed_at.is_some() {
                self.duration_hint = self.completed_at;
            }
            self.current = Some(packet);
            changed = true;
        }
        if changed {
            if let Some(snapshot) = self
                .current
                .as_ref()
                .and_then(|packet| packet.redraw.as_ref())
            {
                let mut frame = snapshot.render(camera.origin, camera.axis)?;
                let rows = |values: &[f32; 16]| {
                    std::array::from_fn::<[f32; 4], 4, _>(|row| {
                        std::array::from_fn(|column| values[row * 4 + column])
                    })
                };
                let projection = self
                    .projection
                    .as_ref()
                    .ok_or("Missing effect frame projector.")?;
                projection.project(
                    &mut frame,
                    camera.origin,
                    rows(&camera.view_projection_rows),
                    rows(&camera.view_rows),
                    rows(&camera.projection_rows),
                )?;
                frame["relativeMatrixRows"] = json!(rows(&camera.relative_view_projection_rows));
                self.frame = frame;
            } else {
                // Terminal/empty packets must never leave an older particle
                // picture stuck on screen. Ordinary scene packets all carry a
                // snapshot, but cancellation or a future producer may not.
                self.frame = Value::Null;
            }
            self.last_camera = Some(camera.clone());
            self.dirty = false;
        }
        Ok(changed)
    }

    pub fn draw_models(&self, _camera: &FxCamera) -> Result<()> {
        if self.controls.enabled && self.frame.is_object() {
            unsafe {
                self.renderer.scene_draw(&self.frame, true).map_err(error)?;
            }
        }
        Ok(())
    }

    /// Set before the model phase as well as before particles. FX model fog must
    /// not inherit a previous frame's environment merely because it draws first.
    pub fn set_environment(
        &mut self,
        camera: &FxCamera,
        float_z: &IDirect3DTexture9,
        fog_consts: [f32; 4],
        fog_color: [f32; 4],
    ) -> Result<()> {
        camera.validate()?;
        if !fog_consts
            .iter()
            .chain(&fog_color)
            .all(|value| value.is_finite())
        {
            return Err("Invalid scene effect fog constants.".into());
        }
        self.renderer.scene_environment(
            camera.width,
            camera.height,
            float_z,
            fog_consts,
            fog_color,
        );
        Ok(())
    }

    pub fn draw_float_z(&self, pass: &super::float_z::FloatZPass<'_>) -> Result<()> {
        if self.controls.enabled && self.frame.is_object() {
            let eye = self
                .last_camera
                .as_ref()
                .ok_or("Missing FX depth camera.")?
                .origin;
            unsafe {
                self.renderer.scene_model_depth(&self.frame, pass, eye)?;
            }
        }
        Ok(())
    }

    pub fn copy_pre_emissive(
        &mut self,
        scene_color: &IDirect3DSurface9,
        width: u32,
        height: u32,
    ) -> Result<()> {
        if self.controls.enabled && self.frame.is_object() {
            unsafe {
                self.renderer
                    .copy_scene(scene_color, width, height)
                    .map_err(error)?;
            }
        }
        Ok(())
    }

    pub fn draw_particles(
        &mut self,
        camera: &FxCamera,
        float_z: &IDirect3DTexture9,
        fog_consts: [f32; 4],
        fog_color: [f32; 4],
    ) -> Result<()> {
        self.set_environment(camera, float_z, fog_consts, fog_color)?;
        if self.controls.enabled && self.frame.is_object() {
            unsafe {
                self.renderer
                    .scene_draw(&self.frame, false)
                    .map_err(error)?;
            }
            self.presented_time = self.frame["time"].as_u64().map(|time| time as u32);
        }
        Ok(())
    }

    pub fn lights(&self) -> &Value {
        &self.frame["lights"]
    }

    pub fn diagnostics(&self) -> Value {
        let sample_settled = self.current.as_ref().is_some_and(|packet| {
            packet.frame["time"]
                .as_f64()
                .is_some_and(|time| time >= self.last_target)
                || self.pending.as_ref().is_some_and(|next| {
                    next.frame["time"]
                        .as_f64()
                        .is_some_and(|time| time > self.last_target)
                })
                || self
                    .completed_at
                    .is_some_and(|end| f64::from(end) <= self.last_target)
        });
        json!({"name":self.prepared.as_ref().map(|effect|&effect.name),"enabled":self.controls.enabled,
            "playing":self.controls.playing,"requestedTimeMs":self.controls.time_ms,
            "timeMs":self.current.as_ref().map(|packet|packet.frame["time"].clone()),"completedAtMs":self.completed_at,
            "clipDurationMs":self.duration_hint,
            "ready":self.frame.is_object(),"sampleSettled":sample_settled,"materialCount":self.renderer.materials_loaded(),
            "drawCount":self.frame["draws"].as_array().map_or(0,|draws|draws.len()),
            "lights":self.lights(),"maxTimeMs":60000,
            "cameraUpdatesPausedGeometry":true})
    }
}
