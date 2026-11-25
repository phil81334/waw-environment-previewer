import { version as appVersion } from '../package.json';
import { createEnvironmentControls } from './environment-controls.js';
import { invoke, isTauri } from '@tauri-apps/api/core';
import { createNativeRenderer, captureNativeProbes } from './native-renderer.js';
import { listen } from '@tauri-apps/api/event';
import { getCurrentWindow } from '@tauri-apps/api/window';
import { createCameraController, cameraAt } from './camera.js';
import { correctCube, resolveCorrection } from './probes/processing.js';
import { validateNativeSettings as validateSettings } from './native-renderer.js';
import {
  parseVision,
  installedBaseline,
  formatVisionValue,
  visionFields,
  validateVisionValue,
} from './vision/state.js';
import { validateFogSettings, formatFogCall } from './fog/state.js';
import { comparisonDefaults, validateComparison, comparisonAngles, fitComparison } from './comparison.js';
import { sceneProfile } from './scene-profile.js';
import './style.css';

const $ = (id) => document.getElementById(id);
const desktop = isTauri();
let probeSettings = sceneProfile.probeSettings;
const presets = Object.fromEntries(sceneProfile.fogPresets.map(({ id, settings }) => [id, settings]));
const environmentControls = createEnvironmentControls({
  onMarkers: (origins) => renderer?.setLightMarkers(origins),
  onChange: () => {
    updateBakeStatus();
    saveSoon();
  },
  onFocus: (light) => focusLight(light).catch(report),
});
let viewportInfoShown = false;
function updateBakeStatus() {
  const dirty = environmentControls.isDirty();
  $('bake-state').textContent = !manifest
    ? 'Bake required'
    : dirty
      ? 'Settings changed · Bake to update'
      : 'Current bake';
  $('bake-state').className = dirty || !manifest ? 'state-chip' : 'state-chip current';
  $('viewport-bake-info').textContent = !manifest
    ? 'No bake loaded'
    : dirty
      ? 'Bake out of date'
      : 'Bake current';
  renderer?.setInfoOverlay({ enabled: viewportInfoShown, dirty: dirty || !manifest });
}
async function focusLight(light) {
  if (!manifest) throw new Error('Bake a scene before viewing a light.');
  const sceneId = manifest.id;
  const eye = await invoke('light_viewpoint', { sceneId, origin: light.origin, reference: sceneProfile.camera.position });
  if (manifest?.id !== sceneId || environmentControls.selectedLight()?.index !== light.index) return;
  camera.setState(cameraAt(eye, light.origin));
  message(`Viewing ${light.label}. Colour / intensity changes require Bake.`, 'success');
}
function setViewportExpanded(expanded) {
  document.body.classList.toggle('viewport-expanded', expanded);
  $('viewport-expand').textContent = expanded ? 'Restore layout' : 'Expand viewport';
  $('viewport-expand').setAttribute('aria-pressed', String(expanded));
  requestAnimationFrame(() => {
    layoutViewport();
    renderer?.render();
  });
}
let comparison = { ...comparisonDefaults };
const activeScene = sceneProfile.id;
let renderer = null;
let installation = null;
let manifest = null;
let busy = false;
let generation = 0;
let controller = null;
let canCancelBake = false;
let fog = validateFogSettings(structuredClone(sceneProfile.fog));
let vision = null;
let visionName = '';
let installedVisionValues = null;
let visionEdited = false;
const visionEditors = new Map();
let saveTimer = null;
let sessionReady = false;
let lastError = '';
let lastRenderError = '';
let lastFrame = performance.now();
const nativePreviewDefaults = {
  time: 0,
  transition: 0,
};
let worldspawnPreview = { ...nativePreviewDefaults };
let skyPlaybackElapsed = 0;
let lastSkyPlaybackFrame = 0;
let skyWindowMinimized = desktop;
let skyWindowCheckPending = false;
let lastSkyWindowCheck = -Infinity;
let fxMetadata = null;
let fxPreparing = false;
let fxPlaying = false;
let fxShown = true;
let fxTime = 0;
let fxRevision = 0;
let fxRestartRevision = 0;
let fxFrameElapsed = 0;
let lastFxFrame = 0;
let fxFailure = '';
let fxRuntimeWarning = '';
let savedCamera = null;
const camera = createCameraController($('viewport'), (value) => {
  savedCamera = value;
  applyCamera(value);
  saveSoon();
});

function applyCamera(value = camera.getState()) {
  renderer?.setCamera({ ...value, fov: comparisonAngles(comparison).vertical });
}
function layoutViewport() {
  const area = $('viewport-wrap');
  const size = fitComparison(comparison, area.clientWidth, area.clientHeight);
  $('viewport-surface').style.width = `${size.width}px`;
  $('viewport-surface').style.height = `${size.height}px`;
}
function refreshComparison() {
  const angles = comparisonAngles(comparison);
  $('camera-fov').value = comparison.cgFov;
  $('camera-framing').value = comparison.mode;
  $('camera-width').value = comparison.width;
  $('camera-height').value = comparison.height;
  $('camera-width').disabled = comparison.mode === 'fill';
  $('camera-height').disabled = comparison.mode === 'fill';
  $('framing-label').textContent =
    `cg_fov ${comparison.cgFov} · ${comparison.mode === 'game' ? `${comparison.width}×${comparison.height}` : 'Fill view'}`;
  $('camera-summary').textContent =
    `Vertical FOV ${angles.vertical.toFixed(2)}°. ${comparison.mode === 'game' ? `Horizontal FOV ${angles.horizontal.toFixed(2)}° at ${comparison.width}×${comparison.height}.` : 'The horizontal view follows the available viewport aspect.'}`;
  layoutViewport();
}
function setComparison(candidate) {
  const next = validateComparison(candidate);
  // Validate graphics dimensions before committing settings or saved state.
  renderer?.setRenderSize(next.mode === 'game' ? { width: next.width, height: next.height } : null);
  comparison = next;
  applyCamera();
  refreshComparison();
  saveSoon();
}
const viewportObserver = new ResizeObserver(layoutViewport);
viewportObserver.observe($('viewport-wrap'));

function sceneObjects() {
  return manifest?.objects ?? [];
}
function objectDrawCount(object, diagnostic) {
  if (!objectVisible(object)) return 0;
  if (object.kind !== 'brush' && diagnostic && Object.hasOwn(diagnostic, object.id))
    return diagnostic[object.id];
  return object.drawCount;
}
function objectVisible(object) {
  if (object.kind === 'brush') return $('show-brushes').checked;
  const script = ['script_model', 'dyn_model'].includes(object.classname);
  return $('show-models').checked && $(script ? 'show-script-models' : 'show-static-models').checked;
}
function applyObjectVisibility() {
  for (const object of sceneObjects()) {
    const visible = objectVisible(object);
    renderer?.setObjectVisibility?.(object.id, visible);
  }
}
function refreshObjectCounts() {
  const objects = sceneObjects();
  const drawCounts = renderer?.getDiagnostics().models?.objectDrawCounts;
  const models = objects.filter((object) => object.kind !== 'brush');
  const brushes = objects.filter((object) => object.kind === 'brush');
  $('object-counts').textContent = manifest
    ? `${models.length} model placements · ${brushes.length} brush entities. ${objects.filter((object) => objectDrawCount(object, drawCounts) > 0).length} objects have visible draws.`
    : 'Build a scene to show its placed objects.';
  const canToggle = typeof renderer?.setObjectVisibility === 'function';
  for (const id of ['show-models', 'show-static-models', 'show-script-models', 'show-brushes'])
    $(id).disabled = !canToggle || !manifest;
}

function message(text, kind = '') {
  $('message').textContent = text;
  $('message').className = `message ${kind}`;
  $('status').textContent = text.length > 115 ? `${text.slice(0, 112)}…` : text;
}
function report(error) {
  lastError = error.message || String(error);
  message(lastError, 'error');
  $('details').open = true;
  updateDetails();
}
function refreshFxControls() {
  const loaded = Number(fxMetadata?.loadedCount ?? 0);
  const total = Number(fxMetadata?.totalCount ?? 0);
  $('fx-enabled').disabled = !loaded || fxPreparing || Boolean(fxRuntimeWarning);
  $('fx-enabled').checked = fxShown;
  $('fx-play').disabled = !loaded || fxPreparing || fxPlaying || Boolean(fxRuntimeWarning);
  $('fx-pause').disabled = !fxPlaying;
  $('fx-loaded').textContent = fxPreparing
    ? 'Loading the built scene’s authored FX…'
    : fxMetadata
      ? `${loaded} of ${total} level FX placements loaded.`
      : 'Build a scene to load its authored FX automatically.';
  $('fx-playback-status').textContent = fxPreparing
    ? 'The FX clock is paused while the level layer is prepared.'
    : `${fxPlaying ? 'Playing' : 'Paused'}. Playback pauses during builds, hidden/minimised windows and dialogs.`;
  const warnings = [
    ...(fxMetadata?.warnings ?? []),
    ...(fxMetadata?.placements ?? []).map((item) => item.warning).filter(Boolean),
  ];
  $('fx-support').textContent =
    fxRuntimeWarning ||
    fxFailure ||
    [...new Set(warnings)].join(' ') ||
    'Authored level FX are loaded automatically. No gameplay scripts are executed.';
}
function applyFxPreview() {
  renderer?.setEffectPreview({
    enabled: Number(fxMetadata?.loadedCount ?? 0) > 0 && fxShown && !fxRuntimeWarning,
    playing: fxPlaying,
    repeat: false,
    timeMs: fxTime * 1000,
    restartRevision: fxRestartRevision,
  });
}
function handleFxWarning(warning) {
  fxRuntimeWarning = warning;
  if (warning) {
    fxPlaying = false;
    fxFrameElapsed = 0;
    applyFxPreview();
    message(warning, 'error');
    $('details').open = true;
  }
  refreshFxControls();
  updateDetails();
}
async function prepareLevelFx(sceneId, currentJob = () => true) {
  const token = ++fxRevision;
  fxPreparing = true;
  fxPlaying = false;
  fxFailure = '';
  fxRuntimeWarning = '';
  applyFxPreview();
  refreshFxControls();
  try {
    const metadata = await invoke('level_fx_prepare', { sceneId });
    if (token !== fxRevision || !currentJob())
      throw new DOMException('Scene changed or cancelled.', 'AbortError');
    if (
      !metadata ||
      !Array.isArray(metadata.placements) ||
      !Number.isInteger(metadata.loadedCount) ||
      !Number.isInteger(metadata.totalCount)
    ) {
      throw new Error('The native level FX response is invalid.');
    }
    fxMetadata = metadata;
    fxTime = 0;
    fxRestartRevision++;
    fxPlaying = metadata.loadedCount > 0;
    fxFrameElapsed = 0;
    lastFxFrame = performance.now();
    lastSkyWindowCheck = -Infinity;
    applyFxPreview();
  } catch (error) {
    if (error.name === 'AbortError' || !currentJob()) throw error;
    fxMetadata = { placements: [], warnings: [], loadedCount: 0, totalCount: 0 };
    fxPlaying = false;
    fxFailure = `Level FX unavailable: ${error.message || error}. The compiled backdrop is retained.`;
    applyFxPreview();
    message(fxFailure, 'error');
  } finally {
    if (token === fxRevision) {
      fxPreparing = false;
      refreshFxControls();
      updateDetails();
    }
  }
}
function buildVisionControls() {
  const groups = new Map();
  for (const field of visionFields) {
    if (!groups.has(field.group)) {
      const group = document.createElement('fieldset'),
        legend = document.createElement('legend');
      legend.textContent = field.group;
      group.append(legend);
      groups.set(field.group, group);
      $('vision-fields').append(group);
    }
    const row = document.createElement(field.type === 'vector' ? 'div' : 'label');
    row.className = field.type === 'bool' ? 'check' : 'field';
    const title = document.createElement('span');
    title.textContent = field.name;
    const inputs = [];
    if (field.type === 'vector') {
      row.append(title);
      const rgb = document.createElement('div');
      rgb.className = 'rgb-row';
      for (let axis = 0; axis < 3; axis++) {
        const label = document.createElement('label');
        label.textContent = ['R', 'G', 'B'][axis];
        const input = document.createElement('input');
        input.type = 'text';
        input.inputMode = 'decimal';
        input.id = `vision-value-${field.name}-${axis}`;
        input.spellcheck = false;
        inputs.push(input);
        label.append(input);
        rgb.append(label);
      }
      row.append(rgb);
    } else {
      const input = document.createElement('input');
      input.type = field.type === 'bool' ? 'checkbox' : 'text';
      input.id = `vision-value-${field.name}`;
      if (field.type !== 'bool') {
        input.inputMode = 'decimal';
        input.spellcheck = false;
      }
      inputs.push(input);
      if (field.type === 'bool') row.append(input, title);
      else row.append(title, input);
    }
    if (field.note) {
      const note = document.createElement('small');
      note.className = 'hint';
      note.textContent = field.note;
      row.append(note);
    }
    for (const input of inputs)
      input.onchange = () => {
        try {
          const number = (element) => (element.value.trim() === '' ? NaN : Number(element.value));
          setVisionField(
            field.name,
            field.type === 'bool'
              ? inputs[0].checked
              : field.type === 'vector'
                ? inputs.map(number)
                : number(inputs[0]),
          );
        } catch (error) {
          report(error);
          refreshVisionControls();
        }
      };
    visionEditors.set(field.name, { field, inputs });
    groups.get(field.group).append(row);
  }
}
function refreshVisionControls() {
  for (const { field, inputs } of visionEditors.values()) {
    inputs.forEach((input, axis) => {
      input.disabled = !vision;
      if (field.type === 'bool') input.checked = Boolean(vision?.[field.name]);
      else
        input.value = vision
          ? formatVisionValue(field.type === 'vector' ? vision[field.name][axis] : vision[field.name])
          : '';
    });
  }
  $('vision-reset-values').disabled = !installedVisionValues || !visionEdited;
}
function setVisionField(name, value) {
  if (!vision) throw new Error('Load an installed Vision preset first.');
  const next = { ...vision, [name]: validateVisionValue(name, value) };
  renderer?.setVisionPreview({
    values: next,
    fade: Number($('revive-fade').value),
    enabled: $('vision-enabled').checked,
  });
  vision = next;
  visionEdited = true;
  refresh();
  message('Vision preview value updated. Installed files are unchanged.', 'success');
}
function action(fn) {
  return async () => {
    try {
      await fn();
    } catch (error) {
      report(error);
    }
  };
}
function validCamera(value) {
  return (
    value &&
    Array.isArray(value.position) &&
    value.position.length === 3 &&
    value.position.every(Number.isFinite) &&
    Number.isFinite(value.yaw) &&
    Number.isFinite(value.pitch)
  );
}
function updateDetails() {
  const captureWarnings = (manifest?.warnings || []).map((warning) =>
    warning.startsWith('First-generation reflection capture:')
      ? 'First-generation reflection capture uses the supported built scene with black seeds, without fog or Vision. Fog is applied in the displayed scene before Vision; fog within captured reflections is not reproduced in this v1.'
      : warning,
  );
  $('details-text').textContent = [
    'Supported effects share the camera and depth buffer of the compiled scene.',
    'Static and script_ placements are rendered without gameplay simulation. Effects share the native scene renderer.',
    fxMetadata
      ? `Level FX: ${fxMetadata.loadedCount} / ${fxMetadata.totalCount} placements loaded.`
      : 'No level FX layer prepared.',
    ...(fxMetadata?.sourceMap ? [`FX placement source: ${fxMetadata.sourceMap}`] : []),
    ...(fxMetadata?.placements ?? []).map(
      (item) => `${item.id} · ${item.name}: ${item.status}${item.warning ? ' · ' + item.warning : ''}`,
    ),
    ...(fxMetadata?.warnings ?? []).map((warning) => `FX warning: ${warning}`),
    ...(fxRuntimeWarning ? [fxRuntimeWarning] : []),
    ...(fxMetadata?.limits ?? []),
    ...(fxFailure ? [fxFailure] : []),
    `Renderer: Direct3D 9 · ${renderer?.getDiagnostics().adapter?.description ?? 'awaiting adapter details'}`,
    `Camera: cg_fov ${comparison.cgFov}; ${comparison.mode === 'game' ? `${comparison.width}x${comparison.height} game frame` : 'fill viewport'}. Match position/direction separately.`,
    `Fog request: ${formatFogCall(fog)}`,
    `Scene reference fog: ${formatFogCall(sceneProfile.fog).trim()}`,
    visionName ? `Installed Vision: ${visionName}.vision` : 'No Vision file loaded.',
    'Revive fade is separate manual preview state, not automatic gameplay timing.',
    manifest
      ? `${manifest.geometry.indexCount / 3} compiled BSP triangles · ${manifest.materials.length} materials · ${manifest.probes.length} reflection probes`
      : 'No compiled scene yet.',
    ...(installation?.installation?.warnings || []),
    ...captureWarnings,
    ...(renderer?.getDiagnostics().models?.warnings || []),
    ...(lastError ? [lastError] : []),
  ].join('\n');
}
function applyPreview() {
  renderer?.setFogPreview(fog);
  if (vision)
    renderer?.setVisionPreview({
      values: vision,
      fade: Number($('revive-fade').value),
      enabled: $('vision-enabled').checked,
    });
  renderer?.setReflectionsEnabled($('reflections').checked);
  renderer?.setWorldspawnPreview(worldspawnPreview);
  applyFxPreview();
  applyObjectVisibility();
}
function refreshNativePreview() {
  const layers = manifest?.sky?.layers ?? [];
  const transition = layers.some((layer) => String(layer.techset).endsWith('transition'));
  const moving = transition || layers.some((layer) => layer.scroll?.some((value) => value !== 0));
  $('native-preview-controls').hidden = !moving;
  $('preview-transition').disabled = !transition;
  if (!moving) $('preview-animate-sky').checked = false;
  for (const [id, key] of [
    ['preview-time', 'time'],
    ['preview-transition', 'transition'],
  ]) {
    $(id).value = worldspawnPreview[key];
  }
}
function setNativePreview({ worldspawn = worldspawnPreview }, announce = true) {
  const next = { ...worldspawnPreview, ...worldspawn };
  for (const key of ['time', 'transition']) {
    if (!Number.isFinite(next[key])) throw new Error(`Invalid ${key} preview value.`);
  }
  if (next.time < 0) throw new Error('Preview time must be zero or greater.');
  if (next.transition < 0 || next.transition > 1) throw new Error('Sky transition must be between 0 and 1.');
  renderer?.setWorldspawnPreview(next);
  worldspawnPreview = next;
  refreshNativePreview();
  if (announce) message('Level sky preview updated. No scene rebuild needed.', 'success');
}
function readNativePreview() {
  const number = (id) => ($(id).value.trim() === '' ? NaN : Number($(id).value));
  return {
    worldspawn: {
      time: number('preview-time'),
      transition: number('preview-transition'),
    },
  };
}
function refresh(syncInputs = false) {
  $('scene-title').textContent = `${sceneProfile.title} · compiled world geometry`;
  $('vision-name').textContent = visionName
    ? `${visionName}.vision · ${visionEdited ? 'preview values edited' : 'installed source'}`
    : 'No Vision loaded.';
  $('height-fields').hidden = fog.mode !== 'volumetric';
  if (syncInputs) {
    $('fog-enabled').checked = fog.enabled;
    $('fog-mode').value = fog.mode;
    for (const [id, key] of [
      ['start-distance', 'startDistance'],
      ['halfway-distance', 'halfwayDistance'],
      ['halfway-height', 'halfwayHeight'],
      ['base-height', 'baseHeight'],
    ]) {
      $(id).value = formatVisionValue(fog[key]);
    }
    ['red', 'green', 'blue'].forEach((component, index) => {
      $(`fog-${component}`).value = formatVisionValue(fog.color[index]);
    });
  }
  $('revive-fade-value').textContent = Number($('revive-fade').value).toFixed(2);
  const ready = desktop && installation?.ready;
  environmentControls.setBusy(busy);
  $('bake').disabled = !ready || busy;
  $('cancel').disabled = !canCancelBake;
  $('load-vision').disabled = !ready;
  $('reset-vision').disabled = !ready;
  $('viewport-label').hidden = !manifest;
  $('viewport-label').textContent =
    `${fog.mode === 'volumetric' ? 'Height' : 'Distance'} fog ${fog.enabled ? 'on' : 'off'} · Vision ${$('vision-enabled').checked ? 'on' : 'off'}`;
  refreshObjectCounts();
  refreshFxControls();
  refreshVisionControls();
  refreshNativePreview();
  updateDetails();
}
function setFog(candidate, note = 'Fog updated immediately. No scene rebuild needed.') {
  const next = validateFogSettings(candidate);
  renderer?.setFogPreview(next);
  fog = next;
  lastError = '';
  refresh(true);
  message(note, 'success');
  saveSoon();
}
function readFogControls() {
  const number = (id) => ($(id).value.trim() === '' ? NaN : Number($(id).value));
  return {
    ...fog,
    enabled: $('fog-enabled').checked,
    mode: $('fog-mode').value,
    startDistance: number('start-distance'),
    halfwayDistance: number('halfway-distance'),
    halfwayHeight: number('halfway-height'),
    baseHeight: number('base-height'),
    transitionSeconds: 0,
    color: ['red', 'green', 'blue'].map((component) => number(`fog-${component}`)),
  };
}
function saveSoon() {
  if (!desktop || !installation?.ready || !sessionReady) return;
  clearTimeout(saveTimer);
  saveTimer = setTimeout(async () => {
    try {
      await invoke('save_session', {
        document: {
          version: 1,
          levelFxScene: true,
          activeScene,
          cameras: { [activeScene]: savedCamera },
          comparison,
          fog,
          visionName,
          visionEnabled: $('vision-enabled').checked,
          reflectionsEnabled: $('reflections').checked,
          reviveFade: Number($('revive-fade').value),
          environmentSettings: environmentControls.snapshot(),
          showStaticModels: $('show-static-models').checked,
          showScriptModels: $('show-script-models').checked,
          showModels: $('show-models').checked,
          showBrushes: $('show-brushes').checked,
        },
      });
    } catch (error) {
      report(error);
    }
  }, 700);
}
async function openVision(name) {
  if (!name) return;
  const result = await invoke('load_vision', { name, saved: false });
  const parsed = parseVision(result.text, installedBaseline(result.defaultText));
  if (!parsed.safe) throw new Error(`${name}.vision cannot be previewed: ${parsed.errors.join(' ')}`);
  renderer?.setVisionPreview({
    values: parsed.values,
    fade: Number($('revive-fade').value),
    enabled: $('vision-enabled').checked,
  });
  vision = parsed.values;
  installedVisionValues = structuredClone(parsed.values);
  visionEdited = false;
  visionName = result.name;
  $('vision-list').value = visionName;
  refresh();
  saveSoon();
}
async function buildScene(allowRestore = false) {
  if (busy || !installation?.ready || !renderer) return;
  const snapshot = structuredClone(probeSettings);
  snapshot.environment = environmentControls.snapshot();
  const errors = validateSettings(snapshot);
  if (errors.length) throw new Error(errors[0]);
  busy = true;
  canCancelBake = true;
  const token = ++generation;
  const abort = new AbortController();
  controller = abort;
  const currentJob = () => token === generation && !abort.signal.aborted;
  const check = () => {
    if (!currentJob()) throw new DOMException('Scene changed or cancelled.', 'AbortError');
  };
  const started = performance.now();
  try {
    refresh();
    message('Building the fixed scene with the original Modtools…');
    const restored =
      allowRestore &&
      (await invoke('restore_scene', { settings: snapshot }).catch((error) => {
        message(`Saved bake unavailable: ${error.message || error}. Building afresh…`);
        return false;
      }));
    check();
    const job = restored ? null : await invoke('begin_bake', { settings: snapshot, revision: token });
    while (!restored) {
      const status = await invoke('bake_status');
      if (status.jobId !== job.jobId) throw new Error('The build job was replaced.');
      if (status.state !== 'running') {
        check();
        if (status.state !== 'ready')
          throw new Error(status.error || status.message || 'Scene build failed.');
        break;
      }
      if (!currentJob()) await invoke('cancel_bake');
      else message(status.message || 'Building…');
      await new Promise((resolve) => setTimeout(resolve, 250));
    }
    check();
    const next = await invoke('scene_manifest');
    check();
    message('Uploading the compiled scene to Direct3D 9…');
    await renderer.prepareScene(next, currentJob);
    check();
    message('Capturing native scene reflections…');
    const raw = await captureNativeProbes(next, {
      signal: abort.signal,
      onProgress: (text) => {
        if (currentJob()) message(text);
      },
    });
    check();
    const rows = installation.installation.corrections;
    const corrected = raw.map((cube, index) =>
      correctCube(
        cube,
        resolveCorrection(snapshot.probes[index].correction || snapshot.globalCorrection || 'default', rows)
          .row,
      ),
    );
    check();
    // Installing the cubes commits the native scene. Finish updating the UI
    // and FX together so both describe the same bake.
    canCancelBake = false;
    $('cancel').disabled = true;
    const loaded = await renderer.commitScene(next, corrected, currentJob);
    check();
    if (!loaded) throw new Error('The completed native scene could not be committed.');
    manifest = next;
    environmentControls.markBaked(snapshot.environment);
    updateBakeStatus();
    refreshObjectCounts();
    applyCamera();
    applyPreview();
    $('empty-view').hidden = true;
    $('scene-stats').textContent =
      `${next.geometry.indexCount / 3} world/brush triangles · ${next.objects?.length ?? 0} placed objects · ${next.probes.length} probes`;
    updateBakeStatus();
    message('Loading the built scene’s authored FX…');
    await prepareLevelFx(next.id, currentJob);
    check();
    if (!restored)
      await invoke('save_scene_cache').catch((error) => {
        lastError = `Scene ready, but saved bake unavailable: ${error.message || error}`;
        updateDetails();
      });
    if (!fxFailure)
      message(
        `${restored ? 'Saved bake restored' : 'Scene ready'} in ${((performance.now() - started) / 1000).toFixed(1)}s · ${fxMetadata?.loadedCount ?? 0} level FX placements loaded.`,
        'success',
      );
  } catch (error) {
    await renderer.abortScene().catch(() => {});
    if (error.name === 'AbortError' || !currentJob())
      message('Build cancelled. The previous complete scene is retained.');
    else report(error);
  } finally {
    busy = false;
    canCancelBake = false;
    controller = null;
    refresh();
  }
}
function wireControls() {
  buildVisionControls();
  $('viewport-expand').onclick = () =>
    setViewportExpanded(!document.body.classList.contains('viewport-expanded'));
  document.addEventListener('keydown', (event) => {
    if (event.key === 'Escape' && document.body.classList.contains('viewport-expanded')) {
      setViewportExpanded(false);
      event.preventDefault();
    }
  });
  $('viewport-info').onclick = () => {
    viewportInfoShown = !viewportInfoShown;
    $('viewport-info').setAttribute('aria-pressed', String(viewportInfoShown));
    $('viewport-info-overlay').hidden = !viewportInfoShown;
    updateBakeStatus();
  };
  $('reset-probes').onclick = () => {
    $('reflections').checked = true;
    applyPreview();
    saveSoon();
    message('Probes visible. Existing capture reused; no bake required.');
  };
  $('reset-vision').onclick = action(async () => {
    $('vision-enabled').checked = true;
    $('revive-fade').value = 0;
    const names = [...$('vision-list').options].map((option) => option.value);
    await openVision(
      names.find((name) => name.toLowerCase() === sceneProfile.defaultVision.toLowerCase()) ||
        names.find((name) => name.toLowerCase() === 'default') ||
        names[0],
    );
    applyPreview();
    refresh();
    saveSoon();
    message('Vision reset immediately to the scene preset.');
  });
  $('reset-entities').onclick = () => {
    for (const id of ['show-models', 'show-static-models', 'show-script-models', 'show-brushes'])
      $(id).checked = true;
    applyObjectVisibility();
    refreshObjectCounts();
    saveSoon();
    message('Entities visible. Existing baked shadows are unchanged.');
  };
  $('reset-fx').onclick = () => {
    fxShown = true;
    fxTime = 0;
    fxRestartRevision++;
    fxPlaying = Number(fxMetadata?.loadedCount ?? 0) > 0;
    $('fx-enabled').checked = true;
    applyFxPreview();
    refreshFxControls();
    message('FX reset immediately.');
  };
  $('environment-reset-world').addEventListener('click', () => {
    $('preview-animate-sky').checked = false;
    skyPlaybackElapsed = 0;
    setNativePreview({ worldspawn: { ...nativePreviewDefaults } }, false);
    updateBakeStatus();
  });
  $('vision-reset-values').onclick = action(() => {
    if (!installedVisionValues) return;
    vision = structuredClone(installedVisionValues);
    visionEdited = false;
    applyPreview();
    refresh();
    message('Vision preview reset to the loaded installed preset.', 'success');
  });
  $('fx-enabled').onchange = () => {
    fxShown = $('fx-enabled').checked;
    applyFxPreview();
    refreshFxControls();
  };
  $('fx-play').onclick = () => {
    if (Number(fxMetadata?.loadedCount ?? 0) > 0 && !fxPreparing && !fxRuntimeWarning) {
      fxPlaying = true;
      fxFrameElapsed = 0;
      lastFxFrame = performance.now();
      lastSkyWindowCheck = -Infinity;
      applyFxPreview();
      refreshFxControls();
    }
  };
  $('fx-pause').onclick = () => {
    fxPlaying = false;
    fxFrameElapsed = 0;
    applyFxPreview();
    refreshFxControls();
  };
  $('preview-animate-sky').onchange = () => {
    skyPlaybackElapsed = 0;
    lastSkyPlaybackFrame = performance.now();
    if ($('preview-animate-sky').checked) lastSkyWindowCheck = -Infinity;
  };
  const applyNativeControls = () => {
    try {
      setNativePreview(readNativePreview());
    } catch (error) {
      report(error);
      refreshNativePreview();
    }
  };
  for (const id of ['preview-time', 'preview-transition']) {
    $(id).onchange = applyNativeControls;
  }
  $('apply-native-preview').onclick = applyNativeControls;
  $('reset-native-preview').onclick = () => {
    $('preview-animate-sky').checked = false;
    skyPlaybackElapsed = 0;
    setNativePreview({ worldspawn: { ...nativePreviewDefaults } });
  };
  refreshNativePreview();
  for (const id of ['show-models', 'show-static-models', 'show-script-models', 'show-brushes']) {
    $(id).onchange = () => {
      applyObjectVisibility();
      refreshObjectCounts();
      saveSoon();
    };
  }

  $('camera-open').onclick = () => {
    refreshComparison();
    $('camera-error').textContent = '';
    $('camera-dialog').showModal();
  };
  $('camera-framing').onchange = () => {
    const fill = $('camera-framing').value === 'fill';
    $('camera-width').disabled = fill;
    $('camera-height').disabled = fill;
  };
  $('camera-apply').onclick = () => {
    try {
      const number = (id) => ($(id).value.trim() === '' ? NaN : Number($(id).value));
      setComparison({
        cgFov: number('camera-fov'),
        width: number('camera-width'),
        height: number('camera-height'),
        mode: $('camera-framing').value,
      });
      $('camera-error').textContent = '';
      message('Camera and framing updated. No scene rebuild needed.', 'success');
    } catch (error) {
      $('camera-error').textContent = error.message || String(error);
    }
  };
  $('camera-reset').onclick = () => {
    try {
      setComparison({ ...comparisonDefaults });
      $('camera-error').textContent = '';
    } catch (error) {
      $('camera-error').textContent = error.message || String(error);
    }
  };

  for (const id of [
    'fog-enabled',
    'fog-mode',
    'start-distance',
    'halfway-distance',
    'halfway-height',
    'base-height',
    'fog-red',
    'fog-green',
    'fog-blue',
  ]) {
    $(id).addEventListener('change', () => {
      try {
        setFog(readFogControls());
      } catch (error) {
        report(error);
        refresh(true);
      }
    });
  }
  $('apply-preset').onclick = action(() =>
    setFog(
      { ...structuredClone(presets[$('fog-preset').value]), enabled: true },
      'Fog preset applied. No scene rebuild needed.',
    ),
  );
  $('reset-fog').onclick = action(() => {
    $('fog-preset').value = 'reference';
    setFog(structuredClone(sceneProfile.fog), 'Fog reset immediately to the scene reference.');
  });
  $('bake').onclick = action(buildScene);
  $('cancel').onclick = action(async () => {
    if (!canCancelBake) return;
    canCancelBake = false;
    $('cancel').disabled = true;
    controller?.abort();
    message('Cancelling…');
    await invoke('cancel_bake');
  });
  $('reset-camera').onclick = () => camera.setState(sceneProfile.camera);
  $('vision-enabled').onchange = action(() => {
    applyPreview();
    refresh();
    saveSoon();
  });
  $('reflections').onchange = action(() => {
    applyPreview();
    saveSoon();
  });
  $('revive-fade').oninput = action(() => {
    const fade = Number($('revive-fade').value);
    if (vision) renderer?.setVisionPreview({ values: vision, fade, enabled: $('vision-enabled').checked });
    $('revive-fade-value').textContent = fade.toFixed(2);
    saveSoon();
  });
  $('load-vision').onclick = action(async () => {
    try {
      await openVision($('vision-list').value);
      message('Vision updated. No scene rebuild needed.', 'success');
    } catch (error) {
      $('vision-list').value = visionName;
      throw error;
    }
  });
  $('about-open').onclick = () => {
    $('about-dialog').showModal();
    requestAnimationFrame(() => {
      $('repository').focus();
      $('repository').select();
    });
  };
  $('repository').onclick = () => $('repository').select();
  $('startup-error').addEventListener('cancel', (event) => event.preventDefault());
  $('quit').onclick = action(() => invoke('quit'));
}
async function start() {
  $('scene-label').textContent = sceneProfile.title;
  $('empty-scene-title').textContent = sceneProfile.title;
  if (sceneProfile.credit) {
    $('scene-credit').textContent = sceneProfile.credit;
    $('scene-credit').hidden = false;
  }
  $('app-version').textContent = appVersion;
  $('about-version').textContent = appVersion;
  $('fog-preset').replaceChildren(...sceneProfile.fogPresets.map(({ id, label }) => new Option(label, id)));
  wireControls();
  renderer = createNativeRenderer($('viewport'), {
    onError: report,
    onEffectWarning: handleFxWarning,
    onDiagnostics: refreshObjectCounts,
  });
  if (desktop) {
    await listen('viewport-camera', (event) => {
      if (validCamera(event.payload)) camera.setState(event.payload);
    });
    await listen('viewport-error', (event) => report(new Error(String(event.payload))));
    await listen('viewport-escape', () => {
      if (document.body.classList.contains('viewport-expanded')) setViewportExpanded(false);
    });
  }
  if (!desktop) {
    $('workspace').hidden = false;
    $('footer').hidden = false;
    $('browser-notice').hidden = false;
    $('environment').textContent = 'Browser preview';
    camera.setState(sceneProfile.camera);
    setComparison(comparison);
    refresh(true);
    return;
  }
  installation = await invoke('startup_info');
  $('app-version').textContent = installation.version;
  $('about-version').textContent = installation.version;
  $('environment').textContent = installation.environment;
  if (!installation.ready) {
    $('startup-message').textContent = installation.issues.join('\n\n');
    $('startup-error').showModal();
    return;
  }
  environmentControls.initialize(installation.installation);
  const authoredProbes = installation.installation.probeDefaults[activeScene];
  if (authoredProbes && !validateSettings(authoredProbes).length) {
    probeSettings = authoredProbes;
  }
  const names = await invoke('list_visions');
  $('vision-list').replaceChildren(...names.map((name) => new Option(name, name)));
  const initial =
    names.find((name) => name.toLowerCase() === sceneProfile.defaultVision.toLowerCase()) ||
    names.find((name) => name.toLowerCase() === 'default') ||
    names[0];
  if (!initial) throw new Error('No installed Vision presets were found.');
  await openVision(initial);
  try {
    const saved = await invoke('load_session');
    if (saved?.version === 1 && (!saved.activeScene || saved.activeScene === activeScene)) {
      if (saved.environmentSettings) environmentControls.restore(saved.environmentSettings);
      const candidate = validateFogSettings({ ...sceneProfile.fog, ...saved.fog });
      if (saved.comparison) comparison = validateComparison(saved.comparison);
      if (saved.levelFxScene === true && validCamera(saved.cameras?.[activeScene])) {
        savedCamera = saved.cameras[activeScene];
      }
      if (typeof saved.visionEnabled === 'boolean') $('vision-enabled').checked = saved.visionEnabled;
      if (typeof saved.reflectionsEnabled === 'boolean') $('reflections').checked = saved.reflectionsEnabled;
      if (Number.isFinite(saved.reviveFade) && saved.reviveFade >= 0 && saved.reviveFade <= 1)
        $('revive-fade').value = saved.reviveFade;
      if (names.includes(saved.visionName) && visionName !== saved.visionName)
        await openVision(saved.visionName);
      fog = candidate;
      if (typeof saved.showModels === 'boolean') {
        if (saved.showStaticModels === undefined) $('show-static-models').checked = saved.showModels;
        if (saved.showScriptModels === undefined) $('show-script-models').checked = saved.showModels;
      }
      $('show-models').checked = true;
      if (typeof saved.showStaticModels === 'boolean')
        $('show-static-models').checked = saved.showStaticModels;
      if (typeof saved.showScriptModels === 'boolean')
        $('show-script-models').checked = saved.showScriptModels;
      if (typeof saved.showBrushes === 'boolean') $('show-brushes').checked = saved.showBrushes;
    }
  } catch (error) {
    message(`Saved session unavailable: ${error.message || error}`);
  }
  if (import.meta.env.DEV && installation.verification) {
    comparison = validateComparison({ ...comparisonDefaults, width: 320, height: 240 });
    if (installation.verification === 'edited') {
      const edits = environmentControls.snapshot();
      if (edits.lights.length) {
        edits.lights[0].color = [1, 0.2, 0.1];
        edits.lights[0].intensity = 0.5;
      }
      const sunlight = edits.worldspawn.properties.find((pair) => pair[0] === 'sunlight');
      if (sunlight) sunlight[1] = '0.6';
      else edits.worldspawn.properties.push(['sunlight', '0.6']);
      const sky = edits.worldspawn.properties.find((pair) => pair[0] === 'skyboxmodel');
      const alternativeSky = installation.installation.skyModels.find((name) => name !== sky?.[1]);
      if (sky && alternativeSky) sky[1] = alternativeSky;
      environmentControls.restore(edits);
    }
  }
  sessionReady = true;
  applyPreview();
  setComparison(comparison);
  $('workspace').hidden = false;
  $('footer').hidden = false;
  camera.setState(savedCamera || sceneProfile.camera);
  refresh(true);
  await buildScene(true);
  if (import.meta.env.DEV && installation.verification) {
    const { verifyViewport } = await import('./verify-viewport.js');
    await verifyViewport({
      mode: installation.verification,
      manifest,
      environmentControls,
      camera,
      renderer,
      fxMetadata,
      setFog,
      applyPreview,
      getError: () => lastError,
      setEffects(shown, time = fxTime) {
        fxPlaying = false;
        fxTime = time;
        fxShown = shown;
        applyFxPreview();
        refreshFxControls();
      },
    });
  }
}
function advanceSkyPreview(now, dt) {
  const playing = $('preview-animate-sky').checked;
  if ((playing || fxPlaying) && desktop && !skyWindowCheckPending && now - lastSkyWindowCheck >= 500) {
    skyWindowCheckPending = true;
    lastSkyWindowCheck = now;
    void getCurrentWindow()
      .isMinimized()
      .then((value) => {
        skyWindowMinimized = value;
      })
      .catch((error) => {
        skyWindowMinimized = true;
        $('preview-animate-sky').checked = false;
        fxPlaying = false;
        applyFxPreview();
        refreshFxControls();
        report(new Error(`Sky playback paused: ${error.message || error}`));
      })
      .finally(() => {
        skyWindowCheckPending = false;
      });
  }
  if (
    !playing ||
    document.hidden ||
    skyWindowMinimized ||
    busy ||
    !manifest ||
    !renderer ||
    $('workspace').hidden ||
    document.querySelector('dialog[open]')
  ) {
    skyPlaybackElapsed = 0;
    lastSkyPlaybackFrame = now;
    return;
  }
  skyPlaybackElapsed += dt;
  if (now - lastSkyPlaybackFrame < 1000 / 30) return;
  worldspawnPreview = { ...worldspawnPreview, time: worldspawnPreview.time + skyPlaybackElapsed };
  skyPlaybackElapsed = 0;
  lastSkyPlaybackFrame = now;
  renderer.setWorldspawnPreview(worldspawnPreview);
  $('preview-time').value = worldspawnPreview.time.toFixed(3);
}
function frame(now) {
  const dt = Math.max(0, Math.min((now - lastFrame) / 1000, 0.1));
  camera.update(dt);
  lastFrame = now;
  advanceSkyPreview(now, dt);
  advanceFxPreview(now, dt);
  try {
    renderer?.render();
  } catch (error) {
    if (lastRenderError !== error.message) {
      lastRenderError = error.message;
      report(error);
    }
  }
  requestAnimationFrame(frame);
}
function advanceFxPreview(now, dt) {
  if (
    !fxPlaying ||
    !Number(fxMetadata?.loadedCount ?? 0) ||
    fxPreparing ||
    busy ||
    !manifest ||
    !renderer ||
    document.hidden ||
    skyWindowMinimized ||
    $('workspace').hidden ||
    document.querySelector('dialog[open]')
  ) {
    fxFrameElapsed = 0;
    lastFxFrame = now;
    return;
  }
  fxFrameElapsed += dt;
  if (now - lastFxFrame < 1000 / 30) return;
  fxTime += fxFrameElapsed;
  fxFrameElapsed = 0;
  lastFxFrame = now;
  if (fxTime >= 60) {
    fxTime %= 60;
    fxRestartRevision++;
  }
  applyFxPreview();
}
void start().catch(async (error) => {
  if (import.meta.env.DEV && installation?.verification) {
    await invoke('verification_report', { document: { success: false, error: String(error) } }).catch(
      () => {},
    );
    await invoke('quit');
    return;
  }
  if ($('workspace').hidden) {
    $('startup-title').textContent = 'Unable to start World at War Environment Previewer';
    $('startup-message').textContent = String(error);
    $('startup-error').showModal();
  } else report(error);
});
requestAnimationFrame(frame);
