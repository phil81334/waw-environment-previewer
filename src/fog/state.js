// The active scene profile supplies the authored defaults for these fields.
const names = new Set([
  'enabled',
  'mode',
  'startDistance',
  'halfwayDistance',
  'halfwayHeight',
  'baseHeight',
  'color',
  'transitionSeconds',
]);

function finiteFloat(value, label) {
  if (typeof value !== 'number' || !Number.isFinite(value) || !Number.isFinite(Math.fround(value))) {
    throw new Error(`${label} must be a finite 32-bit number.`);
  }
  return Math.fround(value);
}

export function validateFogSettings(candidate) {
  if (!candidate || typeof candidate !== 'object' || Array.isArray(candidate)) {
    throw new Error('Fog settings must be an object.');
  }
  if (Object.keys(candidate).some((name) => !names.has(name))) {
    throw new Error('The fog settings contain an unknown field.');
  }
  const value = { ...candidate };
  if (typeof value.enabled !== 'boolean') throw new Error('Show fog must be enabled or disabled.');
  if (!['exponential', 'volumetric'].includes(value.mode)) {
    throw new Error('Choose exponential or volumetric fog.');
  }
  for (const name of [
    'startDistance',
    'halfwayDistance',
    'halfwayHeight',
    'baseHeight',
    'transitionSeconds',
  ]) {
    value[name] = finiteFloat(value[name], name);
  }
  if (value.startDistance < 0) throw new Error('Start distance must be at least zero.');
  if (value.halfwayDistance <= 0) throw new Error('Halfway distance must be greater than zero.');
  if (value.halfwayHeight < 0) throw new Error('Halfway height must be at least zero.');
  if (value.transitionSeconds < 0) throw new Error('Transition time must be at least zero.');
  // Native fog times are signed 32-bit milliseconds. This is a preview safety
  // bound, not an extra bound claimed for the original script wrapper.
  if (Math.fround(value.transitionSeconds * 1000) >= 2147483648) {
    throw new Error('Transition time exceeds the supported signed 32-bit millisecond range.');
  }
  if (!Array.isArray(value.color) || value.color.length !== 3) {
    throw new Error('Fog colour needs three RGB components.');
  }
  value.color = value.color.map((component) => finiteFloat(component, 'Fog colour'));
  if (value.color.some((component) => component < 0 || component > 1)) {
    throw new Error('Each fog colour component must be between zero and one.');
  }
  if (
    !Number.isFinite(Math.fround(1 / value.halfwayDistance)) ||
    !Number.isFinite(Math.fround(value.transitionSeconds * 1000))
  ) {
    throw new Error('These values exceed the supported float32 preview range.');
  }
  return value;
}

function numberText(value) {
  const rounded = Math.fround(value);
  for (let digits = 1; digits <= 9; digits += 1) {
    const text = String(Number(rounded.toPrecision(digits)));
    if (Math.fround(Number(text)) === rounded) return text;
  }
  return String(rounded);
}

export function formatFogCall(settings) {
  const value = validateFogSettings(settings);
  const parameters = [value.startDistance, value.halfwayDistance];
  if (value.mode === 'volumetric') parameters.push(value.halfwayHeight, value.baseHeight);
  parameters.push(...value.color, value.transitionSeconds);
  const name = value.mode === 'volumetric' ? 'SetVolFog' : 'SetExpFog';
  const note = value.enabled ? '' : '// Preview fog is off; this call applies the configured fog.\n';
  return `${note}${name}( ${parameters.map(numberText).join(', ')} );\n`;
}
