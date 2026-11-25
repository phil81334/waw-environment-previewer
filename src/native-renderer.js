import { invoke, isTauri } from '@tauri-apps/api/core';
import { cameraMatrix, basisView, multiply4 } from './camera.js';
import { fogStateFromSettings, packFogState } from './fog/math.js';
import { validateFogSettings } from './fog/state.js';
import { sceneProfile } from './scene-profile.js';
import {
  gradeConstants,
  filmParameters,
  filmPassEnabled,
  glowPassEnabled,
  blurPlan,
  reviveParameters,
  rayProjection,
} from './vision/math.js';

const faceBases = [
  [
    [1, 0, 0],
    [0, 1, 0],
    [0, 0, 1],
  ],
  [
    [-1, 0, 0],
    [0, -1, 0],
    [0, 0, 1],
  ],
  [
    [0, 1, 0],
    [-1, 0, 0],
    [0, 0, 1],
  ],
  [
    [0, -1, 0],
    [1, 0, 0],
    [0, 0, 1],
  ],
  [
    [0, 0, 1],
    [0, 1, 0],
    [-1, 0, 0],
  ],
  [
    [0, 0, -1],
    [0, 1, 0],
    [1, 0, 0],
  ],
];
function captureMatrix(face) {
  const [forward, left, up] = faceBases[face],
    a = 2047 / 2048,
    f = (a * 256) / 258;
  return Array.from(
    multiply4(
      new Float32Array([f, 0, 0, 0, 0, f, 0, 0, 0, 0, a, 1, 0, 0, -4 * a, 0]),
      basisView(
        left.map((n) => -n),
        up,
        forward,
      ),
    ),
  );
}
export function validateNativeSettings(settings) {
  if (
    settings?.scene !== sceneProfile.id ||
    !Array.isArray(settings.probes) ||
    settings.probes.length !== 1
  ) {
    return ['The fixed scene needs one reflection probe.'];
  }
  return [];
}
export async function captureNativeProbes(manifest, { signal, onProgress = () => {} } = {}) {
  const check = () => {
    if (signal?.aborted) throw new DOMException('Capture cancelled.', 'AbortError');
  };
  const frames = [];
  let worker;
  try {
    for (const [index, probe] of manifest.probes.entries()) {
      const faces = [];
      for (let face = 0; face < 6; face++) {
        check();
        onProgress(`Capturing native probe ${index + 1} · face ${face + 1}/6`);
        const bytes = await invoke('viewport_capture_face', {
          origin: probe.origin,
          matrix: captureMatrix(face),
        });
        faces.push(new Uint8Array(bytes));
      }
      frames.push(faces);
    }
    check();
    worker = new Worker(new URL('./probes/worker.js', import.meta.url), { type: 'module' });
    return await new Promise((resolve, reject) => {
      const abort = () => reject(new DOMException('Capture cancelled.', 'AbortError'));
      signal?.addEventListener('abort', abort, { once: true });
      const finish = (fn) => {
        signal?.removeEventListener('abort', abort);
        fn();
      };
      worker.onmessage = ({ data }) => {
        if (data.message) onProgress(data.message);
        if (data.error) finish(() => reject(new Error(data.error)));
        if (data.cubes)
          finish(() => {
            try {
              check();
              resolve(data.cubes);
            } catch (e) {
              reject(e);
            }
          });
      };
      worker.onerror = (e) => finish(() => reject(new Error(e.message || 'Reflection worker failed.')));
      worker.postMessage(
        { frames },
        frames.flat().map((frame) => frame.buffer),
      );
    });
  } finally {
    worker?.terminate();
  }
}

// Filter coefficients depend on the Vision preset and render resolution. FX
// advances at 30 Hz, but moving the clock or camera does not change the kernels.
function prepareVision(vision, width, height) {
  const values = vision.values;
  const full = [width, height];
  const quarter = full.map((size) => Math.max(1, Math.floor(size / 4)));
  function passes(radius, source) {
    const sigma = Math.fround(Math.fround(Math.fround(radius) * Math.fround(height)) * Math.fround(1 / 480));
    return blurPlan([sigma, sigma], source, quarter).map((pass) => ({
      ...pass,
      taps: Array.from(pass.taps),
    }));
  }
  const glowActive = vision.enabled && glowPassEnabled(values);
  const reviveActive = vision.enabled && values.r_reviveFX_enable && values.r_reviveFX_blurRadiusEdge > 1;
  return {
    ...vision,
    grading: Array.from(gradeConstants(filmParameters(values))),
    filmActive: vision.enabled && filmPassEnabled(values),
    glowActive,
    blurPlan: glowActive ? passes(values.r_glowRadius0, quarter) : [],
    quarterSize: quarter,
    reviveBlurPlan: reviveActive ? passes(values.r_reviveFX_blurRadiusEdge, full) : [],
    reviveEdge: Array.from(gradeConstants(reviveParameters(values, 'Edge'))),
    reviveCenter: Array.from(gradeConstants(reviveParameters(values, 'Center'))),
  };
}

export function createNativeRenderer(
  canvas,
  { onError = () => {}, onEffectWarning = () => {}, onDiagnostics = () => {} } = {},
) {
  const desktop = isTauri();
  let active = false,
    hostReady = false,
    scene = null,
    dirty = true,
    flushing = false,
    disposed = false,
    viewRevision = 0,
    lastBounds = '',
    lastError = '';
  let camera = { position: [0, 0, 72], yaw: 0, pitch: 0, fov: 51.0771934667 },
    renderSize = null,
    fog = null,
    vision = null,
    reflectionsEnabled = true;
  let worldspawn = {
    time: 0,
    transition: 0,
  };
  let infoOverlay = { enabled: false, dirty: false };
  let fx = { enabled: false, playing: false, repeat: false, timeMs: 0, restartRevision: 0 };
  let diagnostics = { backend: 'Direct3D9', models: null },
    lastPoll = 0;
  let lastFxWarning = '';
  let visionCache = null;
  let polling = false;
  function acceptDiagnostics(value) {
    diagnostics = value.renderer ?? value;
    onDiagnostics();
    const warning = typeof diagnostics.fxWarning === 'string' ? diagnostics.fxWarning : '';
    if (warning !== lastFxWarning) {
      lastFxWarning = warning;
      onEffectWarning(warning);
    }
  }
  const visibility = new Map();
  function fail(e) {
    const text = e.message || String(e);
    if (text !== lastError) {
      lastError = text;
      onError(new Error(text));
    }
  }
  function setDirty() {
    dirty = true;
    viewRevision++;
  }
  function finiteVector(value, length) {
    return Array.isArray(value) && value.length === length && value.every(Number.isFinite);
  }
  function view() {
    const width = renderSize?.width ?? Math.max(1, canvas.clientWidth),
      height = renderSize?.height ?? Math.max(1, canvas.clientHeight);
    const packed = fog
      ? packFogState(fog.state, camera.position[2], fog.settings.enabled)
      : { fogConsts: [0, 0, 0, 0], fogColor: [0, 0, 0, 0] };
    let preparedVision = null;
    if (vision) {
      if (!visionCache || width !== visionCache.width || height !== visionCache.height) {
        visionCache = { width, height, settings: prepareVision(vision, width, height) };
      }
      preparedVision = {
        ...visionCache.settings,
        ray: rayProjection(
          camera,
          scene?.lights?.find((light) => light.type === 1)?.direction,
          width / height,
        ),
      };
    }
    return {
      infoOverlay: { ...infoOverlay },
      camera: { ...camera, position: [...camera.position] },
      viewProjection: Array.from(cameraMatrix(camera, width / height)),
      renderSize,
      fog: { ...packed, enabled: !!fog?.settings.enabled, state: fog?.state ?? null },
      fogEnabled: !!fog?.settings.enabled,
      ...packed,
      vision: preparedVision,
      reflectionsEnabled,
      worldspawn: { ...worldspawn },
      fx: { ...fx },
    };
  }
  async function flush() {
    if (!desktop || !active || !hostReady || disposed || flushing || !dirty) return;
    flushing = true;
    const revision = viewRevision;
    dirty = false;
    try {
      await invoke('viewport_view', { view: view() });
    } catch (e) {
      fail(e);
    } finally {
      flushing = false;
      if (revision !== viewRevision) dirty = true;
    }
  }
  async function bounds() {
    if (!desktop || !active || disposed) return;
    const rect = canvas.getBoundingClientRect();
    const b = {
      x: rect.x,
      y: rect.y,
      width: rect.width,
      height: rect.height,
      scale: devicePixelRatio || 1,
      visible:
        rect.width > 0 && rect.height > 0 && !document.querySelector('dialog[open]') && !document.hidden,
    };
    const serialized = JSON.stringify(b);
    if (serialized === lastBounds) return;
    lastBounds = serialized;
    try {
      await invoke('viewport_bounds', { bounds: b });
      hostReady = true;
      if (!renderSize) setDirty();
    } catch (e) {
      lastBounds = '';
      fail(e);
    }
  }
  return {
    async prepareScene(manifest, validate = () => true) {
      if (!desktop) throw new Error('The D3D9 viewport requires the desktop app.');
      active = true;
      await bounds();
      await invoke('viewport_view', { view: view() });
      if (!validate()) throw new DOMException('Scene changed.', 'AbortError');
      await invoke('viewport_load_scene', { sceneId: manifest.id });
    },
    async commitScene(manifest, cubes, validate = () => true) {
      if (!validate()) return false;
      await invoke('viewport_probe_cubes', {
        cubes: cubes.map((cube) => cube.map((faces) => faces.map((bytes) => Array.from(bytes)))),
      });
      scene = manifest;
      visibility.clear();
      // A diagnostics read is optional once the GPU has accepted the cubes.
      await invoke('viewport_diagnostics').then(acceptDiagnostics).catch(fail);
      setDirty();
      await flush();
      return true;
    },
    async abortScene() {
      if (!desktop) return;
      await invoke('viewport_abort_scene');
    },
    setInfoOverlay(value) {
      infoOverlay = { enabled: !!value.enabled, dirty: !!value.dirty };
      setDirty();
    },
    setCamera(value) {
      if (
        !finiteVector(value?.position, 3) ||
        ['yaw', 'pitch', 'fov'].some((key) => value[key] !== undefined && !Number.isFinite(value[key]))
      )
        throw new Error('Invalid camera.');
      camera = { ...camera, ...value, position: [...value.position] };
      setDirty();
    },
    setRenderSize(value) {
      if (
        value &&
        (!Number.isInteger(value.width) ||
          !Number.isInteger(value.height) ||
          value.width < 1 ||
          value.height < 1 ||
          value.width > 8192 ||
          value.height > 8192 ||
          value.width * value.height > 8294400)
      )
        throw new Error('The requested render size exceeds the supported graphics limits.');
      renderSize = value ? { ...value } : null;
      setDirty();
    },
    setFogPreview(settings) {
      const checked = validateFogSettings(settings);
      fog = { settings: checked, state: fogStateFromSettings(checked) };
      setDirty();
    },
    setVisionPreview(value) {
      vision = structuredClone(value);
      visionCache = null;
      setDirty();
    },
    setEffectPreview(value) {
      const next = { ...fx, ...value };
      if (
        !Number.isFinite(next.timeMs) ||
        next.timeMs < 0 ||
        next.timeMs > 60000 ||
        !Number.isSafeInteger(next.restartRevision) ||
        next.restartRevision < 0 ||
        ['enabled', 'playing', 'repeat'].some((key) => typeof next[key] !== 'boolean')
      )
        throw new Error('Invalid effect preview state.');
      if (JSON.stringify(next) === JSON.stringify(fx)) return;
      fx = next;
      setDirty();
    },
    setReflectionsEnabled(value) {
      reflectionsEnabled = !!value;
      setDirty();
    },
    setWorldspawnPreview(value) {
      const next = { ...worldspawn, ...value };
      for (const key of ['time', 'transition']) {
        if (!Number.isFinite(next[key])) throw new Error(`Invalid ${key} preview value.`);
      }
      // Time is an explicit preview input. A static scene never starts a timer.
      // A UI playback control can advance it; unchanged time causes no redraw.
      if (JSON.stringify(next) === JSON.stringify(worldspawn)) return;
      worldspawn = next;
      setDirty();
    },
    setObjectVisibility(id, visible) {
      if (visibility.get(id) === visible) return;
      visibility.set(id, visible);
      if (desktop) {
        void invoke('viewport_object_visibility', { id, visible }).catch((error) => {
          if (visibility.get(id) === visible) visibility.delete(id);
          fail(error);
        });
      }
    },
    getDiagnostics() {
      return {
        ...diagnostics,
        camera,
        renderSize,
        sceneId: scene?.id ?? null,
        worldspawn: { ...worldspawn },
      };
    },
    render() {
      if (disposed) return;
      void bounds();
      void flush();
      if (desktop && scene && !polling && !document.hidden && performance.now() - lastPoll > 1000) {
        lastPoll = performance.now();
        polling = true;
        void invoke('viewport_diagnostics')
          .then(acceptDiagnostics)
          .catch(fail)
          .finally(() => {
            polling = false;
          });
      }
    },
    dispose() {
      disposed = true;
    },
  };
}
