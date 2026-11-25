#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

// Hybrid graphics drivers inspect these exports when choosing a performance GPU.
#[no_mangle]
#[used]
pub static NvOptimusEnablement: u32 = 1;
#[no_mangle]
#[used]
pub static AmdPowerXpressRequestHighPerformance: u32 = 1;

mod scene_cache;
mod storage;
mod viewport;

use serde::Serialize;
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::{
    path::PathBuf,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
    time::{Duration, Instant},
};
use tauri::{Emitter, Manager};
use waw_environment_previewer::{
    bake_entities, d3d9::adapter, inspect_entities, level_fx, scene_profile, validate_probes,
    BakedScene, ProbeSettings,
};

const VERSION: &str = env!("CARGO_PKG_VERSION");
const PLACEMENT: &str =
    "Put this executable in your World at War folder, beside CoDWaW.exe, then open it again.";

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct BakeStatus {
    state: String,
    job_id: u64,
    revision: u64,
    message: String,
    error: Option<String>,
    scene_id: Option<String>,
}

struct JobState {
    status: BakeStatus,
    cancel: Option<Arc<AtomicBool>>,
    scene: Option<Arc<BakedScene>>,
    level_fx: Option<Arc<level_fx::LevelFxDocument>>,
    settings: Option<ProbeSettings>,
    game: Option<PathBuf>,
}

#[derive(Clone)]
struct DesktopState {
    jobs: Arc<Mutex<JobState>>,
    data: PathBuf,
    cache: PathBuf,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct StartupInfo {
    version: &'static str,
    environment: &'static str,
    game_root: String,
    ready: bool,
    issues: Vec<String>,
    installation: Option<Value>,
    data_root: String,
    verification: Option<String>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct StartedJob {
    job_id: u64,
    revision: u64,
}

#[tauri::command]
fn quit(window: tauri::WebviewWindow) -> Result<(), String> {
    window.close().map_err(|error| error.to_string())
}

fn candidate_game_root() -> Result<PathBuf, String> {
    if cfg!(debug_assertions) {
        if let Some(path) = std::env::var_os("WAW_ROOT").filter(|path| !path.is_empty()) {
            return Ok(PathBuf::from(path));
        }
        let saved = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../.local/game-path.txt");
        if let Ok(text) = std::fs::read_to_string(saved) {
            if !text.trim().is_empty() {
                return Ok(PathBuf::from(text.trim()));
            }
        }
        return Err("For development, put the folder containing CoDWaW.exe in .local/game-path.txt, or set WAW_ROOT.".into());
    }
    std::env::current_exe()
        .map_err(|error| error.to_string())?
        .parent()
        .map(PathBuf::from)
        .ok_or_else(|| "Cannot locate the executable folder.".into())
}

#[tauri::command]
async fn startup_info(state: tauri::State<'_, DesktopState>) -> Result<StartupInfo, String> {
    let owned = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        // A repeated startup/dependency check must not leave a previously
        // accepted installation usable after a newer check fails.
        owned.jobs.lock().map_err(|error| error.to_string())?.game = None;
        let mut info = StartupInfo {
            version: VERSION,
            environment: if cfg!(debug_assertions) {
                "Development"
            } else {
                "Production"
            },
            game_root: String::new(),
            ready: false,
            issues: Vec::new(),
            installation: None,
            data_root: owned.data.display().to_string(),
            verification: if cfg!(debug_assertions) {
                std::env::var("WAW_ENV_VERIFY_MODE").ok()
            } else {
                None
            },
        };
        let root = match candidate_game_root() {
            Ok(path) => path,
            Err(error) => {
                info.issues.push(error);
                return Ok(info);
            }
        };
        info.game_root = root.display().to_string();
        if !root.join("CoDWaW.exe").is_file() {
            info.issues.push(PLACEMENT.into());
            return Ok(info);
        }
        match inspect_entities(&root) {
            Ok(installation) => {
                info.installation =
                    Some(serde_json::to_value(installation).map_err(|error| error.to_string())?);
                info.ready = true;
                owned.jobs.lock().map_err(|error| error.to_string())?.game = Some(root);
            }
            Err(error) => info.issues.push(error),
        }
        Ok(info)
    })
    .await
    .map_err(|error| error.to_string())?
}

#[tauri::command]
fn begin_bake(
    settings: ProbeSettings,
    revision: u64,
    state: tauri::State<'_, DesktopState>,
) -> Result<StartedJob, String> {
    validate_probes(&settings)?;
    let owned = state.inner().clone();
    let cancel = Arc::new(AtomicBool::new(false));
    let (game, job_id) = {
        let mut guard = owned.jobs.lock().map_err(|error| error.to_string())?;
        if guard.status.state == "running" {
            return Err("A bake is already running. Cancel it or wait for it to finish.".into());
        }
        let game = guard
            .game
            .clone()
            .ok_or("Open this app beside a valid WaW and Modtools installation first.")?;
        let job_id = guard
            .status
            .job_id
            .checked_add(1)
            .ok_or("Bake counter overflow.")?;
        guard.status = BakeStatus {
            state: "running".into(),
            job_id,
            revision,
            message: "Preparing private bake…".into(),
            error: None,
            scene_id: None,
        };
        guard.cancel = Some(cancel.clone());
        (game, job_id)
    };
    std::thread::spawn(move || {
        // The baker creates its own job folder. Keep the parent path short
        // enough for the original Modtools path buffers.
        let folder = owned.cache.clone();
        let report = |message: String| {
            if let Ok(mut guard) = owned.jobs.lock() {
                if guard.status.job_id == job_id {
                    guard.status.message = message;
                }
            }
        };
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let effects = level_fx::discover()?;
            let scene = bake_entities(&game, &folder, &settings, &cancel, &report)?;
            Ok::<_, String>((scene, effects))
        }))
        .unwrap_or_else(|_| {
            Err("The native bake stopped unexpectedly. The previous scene is retained.".into())
        });
        if let Ok(mut guard) = owned.jobs.lock() {
            if guard.status.job_id != job_id {
                return;
            }
            guard.cancel = None;
            if cancel.load(Ordering::Acquire) {
                guard.status.state = "cancelled".into();
                guard.status.message = "Bake cancelled. Previous scene retained.".into();
            } else {
                match result {
                    Ok((scene, effects)) => {
                        guard.status.scene_id = Some(scene.manifest.id.clone());
                        guard.settings = Some(settings.clone());
                        guard.level_fx = Some(Arc::new(effects));
                        guard.scene = Some(Arc::new(scene));
                        guard.status.state = "ready".into();
                        guard.status.message = "Bake ready.".into();
                    }
                    Err(error) => {
                        guard.status.state = "error".into();
                        guard.status.message = "Bake failed. Previous scene retained.".into();
                        guard.status.error = Some(error);
                    }
                }
            }
        }
    });
    Ok(StartedJob { job_id, revision })
}

#[tauri::command]
fn verification_report(document: Value) -> Result<(), String> {
    if !cfg!(debug_assertions) {
        return Err("Development verification only.".into());
    }
    let path = PathBuf::from(
        std::env::var_os("WAW_ENV_VERIFY_OUTPUT").ok_or("No verification output configured.")?,
    );
    let base = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../.local/verification");
    std::fs::create_dir_all(&base).map_err(|e| e.to_string())?;
    let parent = path
        .parent()
        .ok_or("Invalid verification output.")?
        .canonicalize()
        .map_err(|e| e.to_string())?;
    if parent != base.canonicalize().map_err(|e| e.to_string())? || path.exists() {
        return Err("Verification requires a fresh direct child of .local/verification.".into());
    }
    std::fs::create_dir(&path).map_err(|e| e.to_string())?;
    let bytes = serde_json::to_vec(&document).map_err(|e| e.to_string())?;
    if bytes.len() > 8 * 1024 * 1024 {
        return Err("Verification report exceeds limit.".into());
    }
    std::fs::write(path.join("report.json"), bytes).map_err(|e| e.to_string())
}

#[tauri::command]
async fn restore_scene(
    settings: ProbeSettings,
    state: tauri::State<'_, DesktopState>,
) -> Result<bool, String> {
    validate_probes(&settings)?;
    let owned = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        // A slow cache read must not replace a newer bake that completed
        // while the snapshot was being verified.
        let (game, job_id) = {
            let jobs = owned.jobs.lock().map_err(|e| e.to_string())?;
            if jobs.status.state == "running" {
                return Err("A bake is already running.".into());
            }
            let game = jobs
                .game
                .clone()
                .ok_or("Open the previewer beside CoDWaW.exe first.")?;
            (game, jobs.status.job_id)
        };
        let cached = scene_cache::load(&owned.cache.join("snapshots"), &game, &settings)?;
        let Some((scene, effects)) = cached else {
            return Ok(false);
        };
        let mut guard = owned.jobs.lock().map_err(|e| e.to_string())?;
        if guard.status.job_id != job_id || guard.game.as_ref() != Some(&game) {
            return Err("A newer bake or installation check superseded this saved scene.".into());
        }
        guard.status.scene_id = Some(scene.manifest.id.clone());
        guard.status.state = "ready".into();
        guard.status.message = "Restored last matching bake.".into();
        guard.scene = Some(Arc::new(scene));
        guard.level_fx = Some(Arc::new(effects));
        guard.settings = Some(settings);
        Ok(true)
    })
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
async fn save_scene_cache(state: tauri::State<'_, DesktopState>) -> Result<(), String> {
    let owned = state.inner().clone();
    let game = accepted_game(&owned)?;
    let (scene, settings, effects) = {
        let guard = owned.jobs.lock().map_err(|e| e.to_string())?;
        (
            guard.scene.clone().ok_or("No baked scene.")?,
            guard.settings.clone().ok_or("No baked settings.")?,
            guard.level_fx.clone().ok_or("No scene FX metadata.")?,
        )
    };
    tauri::async_runtime::spawn_blocking(move || {
        scene_cache::save(
            &owned.cache.join("snapshots"),
            &game,
            &scene,
            &settings,
            &effects,
        )
    })
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
fn bake_status(state: tauri::State<DesktopState>) -> Result<BakeStatus, String> {
    Ok(state
        .jobs
        .lock()
        .map_err(|error| error.to_string())?
        .status
        .clone())
}

#[tauri::command]
fn cancel_bake(state: tauri::State<DesktopState>) -> Result<(), String> {
    let mut guard = state.jobs.lock().map_err(|error| error.to_string())?;
    if let Some(cancel) = &guard.cancel {
        cancel.store(true, Ordering::Release);
        guard.status.message = "Cancelling bake…".into();
    }
    Ok(())
}

#[tauri::command]
fn scene_manifest(state: tauri::State<DesktopState>) -> Result<Value, String> {
    let guard = state.jobs.lock().map_err(|error| error.to_string())?;
    let scene = guard.scene.as_ref().ok_or("Bake a scene first.")?;
    serde_json::to_value(&scene.manifest).map_err(|error| error.to_string())
}

#[tauri::command]
fn list_visions(state: tauri::State<DesktopState>) -> Result<Vec<String>, String> {
    let game = accepted_game(&state)?;
    storage::list_visions(&game.join("raw/vision"))
}

fn accepted_game(state: &DesktopState) -> Result<PathBuf, String> {
    state
        .jobs
        .lock()
        .map_err(|error| error.to_string())?
        .game
        .clone()
        .ok_or_else(|| "Open this app beside a valid WaW and Modtools installation first.".into())
}

#[tauri::command]
fn load_vision(
    name: String,
    saved: Option<bool>,
    state: tauri::State<DesktopState>,
) -> Result<storage::VisionSource, String> {
    let game = accepted_game(&state)?;
    if saved.unwrap_or(false) {
        return Err("The previewer reads installed Vision presets only.".into());
    }
    storage::load_vision(&game, &name)
}

#[tauri::command]
fn save_session(document: Value, state: tauri::State<DesktopState>) -> Result<(), String> {
    storage::save_session(&state.data, document)
}

#[tauri::command]
fn load_session(state: tauri::State<DesktopState>) -> Result<Option<Value>, String> {
    storage::load_session(&state.data)
}

/// All Direct3D9/Win32 work stays on the event-loop thread. Commands transfer
/// plain packets or immutable scenes; native COM objects never cross threads.
async fn on_ui<T: Send + 'static>(
    app: &tauri::AppHandle,
    work: impl FnOnce() -> Result<T, String> + Send + 'static,
) -> Result<T, String> {
    let (sender, receiver) = std::sync::mpsc::sync_channel(1);
    app.run_on_main_thread(move || {
        let _ = sender.send(work());
    })
    .map_err(|e| e.to_string())?;
    tauri::async_runtime::spawn_blocking(move || receiver.recv().map_err(|e| e.to_string())?)
        .await
        .map_err(|e| e.to_string())?
}
#[tauri::command]
async fn level_fx_prepare(
    app: tauri::AppHandle,
    state: tauri::State<'_, DesktopState>,
    scene_id: String,
) -> Result<Value, String> {
    let game = accepted_game(&state)?;
    let document = {
        let jobs = state.jobs.lock().map_err(|e| e.to_string())?;
        if jobs
            .scene
            .as_ref()
            .is_none_or(|scene| scene.manifest.id != scene_id)
        {
            return Err("The source scene was replaced; load the current level snapshot.".into());
        }
        jobs.level_fx
            .clone()
            .ok_or("The scene has no captured level FX sources.")?
    };
    let prepared =
        tauri::async_runtime::spawn_blocking(move || level_fx::prepare(&game, &document))
            .await
            .map_err(|e| e.to_string())??;
    on_ui(&app, move || viewport::load_level_fx(&scene_id, prepared)).await
}
#[tauri::command]
async fn viewport_bounds(
    app: tauri::AppHandle,
    state: tauri::State<'_, DesktopState>,
    bounds: viewport::Bounds,
) -> Result<(), String> {
    let game = accepted_game(&state)?;
    let parent = app
        .get_webview_window("main")
        .ok_or("The window is closed.")?
        .hwnd()
        .map_err(|e| e.to_string())?
        .0 as isize;
    on_ui(&app, move || viewport::set_bounds(parent, &game, bounds)).await
}
#[tauri::command]
async fn viewport_load_scene(
    app: tauri::AppHandle,
    state: tauri::State<'_, DesktopState>,
    scene_id: String,
) -> Result<Value, String> {
    let scene = {
        let guard = state.jobs.lock().map_err(|e| e.to_string())?;
        let scene = guard.scene.as_ref().ok_or("Build a scene first.")?;
        if scene.manifest.id != scene_id {
            return Err("The built scene was replaced; load the current scene.".into());
        }
        scene.clone()
    };
    on_ui(&app, move || viewport::load_scene(scene)).await
}
#[tauri::command]
async fn viewport_view(app: tauri::AppHandle, view: Value) -> Result<(), String> {
    on_ui(&app, move || viewport::set_view(view)).await
}
#[tauri::command]
async fn viewport_object_visibility(
    app: tauri::AppHandle,
    id: String,
    visible: bool,
) -> Result<(), String> {
    on_ui(&app, move || viewport::set_object_visibility(&id, visible)).await
}
#[tauri::command]
async fn viewport_diagnostics(app: tauri::AppHandle) -> Result<Value, String> {
    let fullscreen = app
        .get_webview_window("main")
        .and_then(|window| window.is_fullscreen().ok())
        .unwrap_or(false);
    let mut diagnostics = on_ui(&app, || Ok(viewport::diagnostics())).await?;
    diagnostics["monitorFullscreen"] = serde_json::json!(fullscreen);
    Ok(diagnostics)
}
#[tauri::command]
async fn viewport_capture(app: tauri::AppHandle) -> Result<Value, String> {
    on_ui(&app, viewport::capture).await
}
#[tauri::command]
async fn viewport_capture_face(
    app: tauri::AppHandle,
    origin: [f32; 3],
    matrix: [f32; 16],
) -> Result<tauri::ipc::Response, String> {
    let bytes = on_ui(&app, move || viewport::capture_face(origin, matrix)).await?;
    Ok(tauri::ipc::Response::new(bytes))
}
#[tauri::command]
async fn viewport_probe_cubes(app: tauri::AppHandle, cubes: Value) -> Result<(), String> {
    on_ui(&app, move || viewport::set_probe_cubes(cubes)).await
}
#[tauri::command]
async fn viewport_abort_scene(app: tauri::AppHandle) -> Result<(), String> {
    on_ui(&app, viewport::abort_scene).await
}

fn main() {
    if std::env::args().any(|a| a == "--list-d3d9-adapters") {
        match adapter::inventory() {
            Ok(info) => println!("{info}"),
            Err(error) => {
                eprintln!("{error}");
                std::process::exit(1);
            }
        }
        return;
    }
    tauri::Builder::default()
        .setup(|app| {
            let data = app
                .path()
                .app_data_dir()
                .map_err(|error| error.to_string())?;
            let session_folder =
                if cfg!(debug_assertions) && std::env::var_os("WAW_ENV_VERIFY_MODE").is_some() {
                    "verification-v1"
                } else {
                    "environment-v1"
                };
            // Light overrides use their order in the MAP. A new scene must
            // start with its own edits, camera, Vision and fog settings.
            let mut scene_hash = Sha256::new();
            scene_hash.update(scene_profile::SCENE_MAP.as_bytes());
            scene_hash.update([0]);
            scene_hash.update(scene_profile::PROFILE_JSON.as_bytes());
            let data = storage::resolve_data_root(
                &data
                    .join(session_folder)
                    .join(format!("{:x}", scene_hash.finalize())),
            )?;
            let cache = app
                .path()
                .app_cache_dir()
                .map_err(|error| error.to_string())?
                .join("jobs");
            app.manage(DesktopState {
                data,
                cache,

                jobs: Arc::new(Mutex::new(JobState {
                    status: BakeStatus {
                        state: "idle".into(),
                        job_id: 0,
                        revision: 0,
                        message: "Ready.".into(),
                        error: None,
                        scene_id: None,
                    },
                    cancel: None,
                    scene: None,
                    level_fx: None,
                    settings: None,
                    game: None,
                })),
            });
            let pending = Arc::new(AtomicBool::new(false));
            let handle = app.handle().clone();
            std::thread::spawn(move || loop {
                std::thread::sleep(Duration::from_millis(16));
                if pending.swap(true, Ordering::AcqRel) {
                    continue;
                }
                let complete = pending.clone();
                let events = handle.clone();
                if handle
                    .run_on_main_thread(move || {
                        if viewport::take_escape() {
                            let _ = events.emit("viewport-escape", ());
                        }
                        let (camera, error) = viewport::tick();
                        if let Some(camera) = camera {
                            let _ = events.emit("viewport-camera", camera);
                        }
                        if let Some(error) = error {
                            let _ = events.emit("viewport-error", error);
                        }
                        complete.store(false, Ordering::Release);
                    })
                    .is_err()
                {
                    break;
                }
            });
            Ok(())
        })
        .on_window_event(|window, event| {
            if matches!(event, tauri::WindowEvent::Destroyed) {
                viewport::shutdown();
            }
            if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                let state = window.state::<DesktopState>().inner().clone();
                let running = if let Ok(guard) = state.jobs.lock() {
                    if let Some(cancel) = &guard.cancel {
                        cancel.store(true, Ordering::Release);
                        true
                    } else {
                        false
                    }
                } else {
                    false
                };
                if running {
                    // Give the native cancellation loop time to kill/reap its
                    // child compiler before the application process exits.
                    api.prevent_close();
                    let handle = window.app_handle().clone();
                    std::thread::spawn(move || {
                        let start = Instant::now();
                        while start.elapsed() < Duration::from_secs(10) {
                            let active = state
                                .jobs
                                .lock()
                                .map(|guard| guard.cancel.is_some())
                                .unwrap_or(false);
                            if !active {
                                break;
                            }
                            std::thread::sleep(Duration::from_millis(50));
                        }
                        handle.exit(0);
                    });
                }
            }
        })
        .invoke_handler(tauri::generate_handler![
            quit,
            startup_info,
            verification_report,
            begin_bake,
            restore_scene,
            save_scene_cache,
            bake_status,
            cancel_bake,
            scene_manifest,
            list_visions,
            load_vision,
            save_session,
            load_session,
            level_fx_prepare,
            viewport_bounds,
            viewport_load_scene,
            viewport_view,
            viewport_object_visibility,
            viewport_diagnostics,
            viewport_capture,
            viewport_capture_face,
            viewport_probe_cubes,
            viewport_abort_scene
        ])
        .run(tauri::generate_context!())
        .expect("Cannot start World at War Environment Previewer");
}
