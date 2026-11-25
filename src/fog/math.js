import { validateFogSettings } from './state.js';

// Original sp_tool SHA256 3c23551ad411cfc0d77484baaa5a5b413ec236ee4cf06245a368e3bf86b8ed1f.
// 4D1415/4D1618 ->4D1265 ->440C6F (server);5B0EB0 (clientscript);
// 644660 (effective-state interpolation);69EF90 (renderer constants).
const f32 = Math.fround;
const paths = new Set(['global', 'player', 'clientscript']);
const inv255 = f32(1 / 255);

function pathCheck(path) {
  if (!paths.has(path)) throw new Error('Unknown fog execution path.');
}

// Both global and player SERVER calls serialize seven float32 values using
// printf %g (six significant digits). Clientscript calls avoid this transport.
function serverFloat(value) {
  return f32(Number(f32(value).toPrecision(6)));
}

export function fogDurationMilliseconds(seconds, { path = 'global' } = {}) {
  pathCheck(path);
  const milliseconds = f32(f32(seconds) * 1000);
  if (!Number.isFinite(milliseconds) || milliseconds < 0 || milliseconds > 2147483647) {
    throw new Error('Fog transition exceeds the supported signed 32-bit millisecond range.');
  }
  // The server uses %.0f, the clientscript conversion truncates. The legacy
  // formatter's exact halfway tie rule is not established; this preview uses
  // round-to-nearest with positive ties upward for the server path.
  return path === 'clientscript' ? Math.trunc(milliseconds) : Math.round(milliseconds);
}

export function clearFogState({ path = 'global', durationMilliseconds = 0 } = {}) {
  pathCheck(path);
  return {
    enabled: false,
    startDistance: 0,
    density: 0,
    heightDensity: 0,
    baseHeight: 0,
    colorBytes: [0, 0, 0, 0],
    durationMilliseconds,
    path,
  };
}

export function fogStateFromSettings(settings, { path = 'global' } = {}) {
  pathCheck(path);
  const value = validateFogSettings(settings);
  const durationMilliseconds = fogDurationMilliseconds(value.transitionSeconds, { path });
  if (!value.enabled) return clearFogState({ path, durationMilliseconds });
  const transport = path === 'clientscript' ? f32 : serverFloat;
  const heightDensity =
    value.mode === 'volumetric' && value.halfwayHeight >= 1 ? f32(1 / value.halfwayHeight) : 0;
  // Original color conversion stores the product to float32, adds double
  // 2^-30, and fistp under nearest rounding. For supported [0,1] colors this
  // is equivalent to Math.round of that float32 product.
  const colorBytes = value.color.map((component) => Math.round(f32(transport(component) * 255)));
  return {
    enabled: true,
    startDistance: transport(value.startDistance),
    density: transport(f32(1 / value.halfwayDistance)),
    heightDensity: transport(heightDensity),
    baseHeight: value.mode === 'volumetric' ? transport(value.baseHeight) : 0,
    colorBytes: [...colorBytes, 255],
    durationMilliseconds,
    path,
  };
}

function validateEffectiveState(state) {
  if (
    !state ||
    typeof state.enabled !== 'boolean' ||
    !Array.isArray(state.colorBytes) ||
    state.colorBytes.length !== 4 ||
    state.colorBytes.some((component) => !Number.isInteger(component) || component < 0 || component > 255)
  ) {
    throw new Error('Fog effective state needs an enabled flag and four valid color bytes.');
  }
  for (const name of ['startDistance', 'density', 'heightDensity', 'baseHeight']) {
    if (!Number.isFinite(state[name]) || !Number.isFinite(f32(state[name]))) {
      throw new Error('Fog effective state exceeds the supported float32 range.');
    }
  }
  if (state.startDistance < 0 || state.density < 0 || state.heightDensity < 0) {
    throw new Error('Fog effective distances and densities must not be negative.');
  }
}

export function packFogState(state, eyeZ, enabled = true) {
  validateEffectiveState(state);
  if (!Number.isFinite(eyeZ) || !Number.isFinite(f32(eyeZ))) {
    throw new Error('Fog camera height must be a finite 32-bit number.');
  }
  if (!enabled || !state.enabled) return { fogConsts: [0, 0, 0, 0], fogColor: [0, 0, 0, 0] };
  const eyeHeight = f32(f32(eyeZ) - state.baseHeight);
  const exponent = f32(-f32(state.heightDensity * eyeHeight));
  const fogConsts = [
    state.heightDensity,
    state.density,
    f32(2 ** exponent),
    f32(state.startDistance * state.density),
  ];
  if (fogConsts.some((component) => !Number.isFinite(component))) {
    throw new Error('Fog values exceed the supported range at this camera height.');
  }
  return { fogConsts, fogColor: state.colorBytes.map((component) => f32(component * inv255)) };
}
