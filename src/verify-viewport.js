// Desktop regression checks. Vite drops the import from release builds.
import { invoke } from '@tauri-apps/api/core';
import { sceneProfile } from './scene-profile.js';

const $ = (id) => document.getElementById(id);
const denseFog = {
  ...sceneProfile.fog,
  mode: 'volumetric',
  startDistance: 0,
  halfwayDistance: 320,
  halfwayHeight: 200,
};
const delay = (milliseconds) => new Promise((resolve) => setTimeout(resolve, milliseconds));

export async function verifyViewport({
  mode,
  manifest,
  environmentControls,
  camera,
  renderer,
  fxMetadata,
  setEffects,
  setFog,
  applyPreview,
  getError,
}) {
  await delay(1800);
  const capture = manifest ? await invoke('viewport_capture') : null;
  const checks = {};
  if (manifest && mode === 'review') {
    setEffects(true, 1);
    setFog({ ...denseFog, enabled: true });
    applyPreview();
    await delay(600);
    checks.denseFxOn = await invoke('viewport_capture');
    setEffects(false);
    await delay(400);
    checks.denseFxOff = await invoke('viewport_capture');
    setEffects(true);
    setFog({ ...denseFog, enabled: false });
    applyPreview();
    await delay(400);
    checks.noFogFxOn = await invoke('viewport_capture');
    const normalBounds = $('viewport').getBoundingClientRect().toJSON();
    $('viewport-info').click();
    $('viewport-expand').click();
    await delay(400);
    checks.expanded = {
      bounds: $('viewport').getBoundingClientRect().toJSON(),
      native: await invoke('viewport_diagnostics'),
      fullscreen: document.fullscreenElement !== null,
      normalBounds,
    };
    $('viewport-expand').click();
    $('reset-probes').click();
    $('reset-entities').click();
    $('reset-fx').click();
    $('reset-fog').click();
    await $('reset-vision').onclick();
    const cameraBeforeFocus = camera.getState();
    $('environment-light-panel').open = true;
    $('environment-light-list').selectedIndex = Math.min(1, $('environment-light-list').options.length - 1);
    $('environment-light-list').onchange();
    $('environment-view-light').click();
    await delay(400);
    checks.focus = {
      selectedLight: environmentControls.selectedLight(),
      cameraBeforeFocus,
      camera: camera.getState(),
      capture: await invoke('viewport_capture'),
    };
    if (JSON.stringify(checks.focus.camera.position) === JSON.stringify(cameraBeforeFocus.position))
      throw new Error('Light-focus verification: camera did not move.');
    $('environment-light-panel').open = false;
    const intensity = $('environment-light-intensity');
    intensity.value = String(Number(intensity.value) === 0.5 ? 0.75 : 0.5);
    intensity.onchange({ target: intensity });
    await delay(300);
    checks.dirtyBake = {
      text: $('bake-state').textContent,
      dirty: environmentControls.isDirty(),
      native: await invoke('viewport_diagnostics'),
    };
    if (!checks.dirtyBake.dirty || !checks.dirtyBake.native.infoOverlay.dirty)
      throw new Error('Bake-status verification did not mark changed settings.');
    $('environment-reset-lights').click();
    checks.sections = [...$('controls').children].map((section) => ({
      name: section.querySelector('summary')?.childNodes[0].textContent.trim(),
      tag: section.tagName,
      open: section.open,
    }));
    checks.resets = {
      probes: $('reflections').checked,
      vision: $('vision-enabled').checked,
      fog: $('fog-enabled').checked,
      static: $('show-static-models').checked,
      script: $('show-script-models').checked,
      fx: $('fx-enabled').checked,
    };
  }
  if (manifest && mode === 'edited') {
    setEffects(true);
    $('vision-enabled').checked = false;
    setFog({ ...denseFog, enabled: true });
    $('show-models').checked = false;
    $('reflections').checked = false;
    setEffects(false);
    applyPreview();
    await delay(500);
    checks.togglesOff = {
      capture: await invoke('viewport_capture'),
      diagnostics: renderer.getDiagnostics(),
    };
    $('show-models').checked = true;
    $('vision-enabled').checked = true;
    $('reflections').checked = true;
    setEffects(true);
    setFog(sceneProfile.fog);
    applyPreview();
  }
  await invoke('verification_report', {
    document: {
      success: Boolean(manifest) && !getError(),
      mode,
      lastError: getError(),
      message: $('message').textContent,
      settings: environmentControls.snapshot(),
      worldspawn: manifest?.worldspawn,
      lights: manifest?.lights,
      warnings: manifest?.warnings,
      fx: fxMetadata,
      diagnostics: renderer.getDiagnostics(),
      capture,
      checks,
    },
  });
  await invoke('quit');
}
