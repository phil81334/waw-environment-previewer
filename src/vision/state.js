/** WaW Vision text/state handling. The editor uses a predictable zero + installed
 * default baseline rather than the game's stale eight-slot cache. */
const definitions = [
  ['r_glow', 'bool', 'Glow'],
  ['r_glowBloomCutoff', 'float', 'Glow'],
  ['r_glowBloomDesaturation', 'float', 'Glow'],
  ['r_glowBloomIntensity0', 'float', 'Glow'],
  ['r_glowBloomIntensity1', 'float', 'Legacy glow', 'Preserved; inactive in the verified normal renderer.'],
  ['r_glowRadius0', 'float', 'Glow'],
  ['r_glowRadius1', 'float', 'Legacy glow', 'Preserved; inactive in the verified normal renderer.'],
  [
    'r_glowSkyBleedIntensity0',
    'float',
    'Legacy glow',
    'Preserved; inactive in the verified normal renderer.',
  ],
  [
    'r_glowSkyBleedIntensity1',
    'float',
    'Legacy glow',
    'Preserved; inactive in the verified normal renderer.',
  ],
  ['r_glowRayExpansion', 'float', 'Glow'],
  ['r_glowRayIntensity', 'float', 'Glow'],
  ['r_filmEnable', 'bool', 'Film'],
  ['r_filmBrightness', 'float', 'Film'],
  ['r_filmContrast', 'float', 'Film'],
  ['r_filmDesaturation', 'float', 'Film'],
  ['r_filmInvert', 'bool', 'Film'],
  ['r_filmLightTint', 'vector', 'Film'],
  ['r_filmDarkTint', 'vector', 'Film'],
  ['r_reviveFX_enable', 'bool', 'Revive'],
  ['r_reviveFX_contrastEdge', 'float', 'Revive edge'],
  ['r_reviveFX_brightnessEdge', 'float', 'Revive edge'],
  ['r_reviveFX_desaturationEdge', 'float', 'Revive edge'],
  ['r_reviveFX_darkTintEdge', 'vector', 'Revive edge'],
  ['r_reviveFX_lightTintEdge', 'vector', 'Revive edge'],
  ['r_reviveFX_blurRadiusEdge', 'float', 'Revive'],
  [
    'r_reviveFX_motionblurWeight',
    'float',
    'Preserved motion',
    'Preserved; no history blend in the verified normal Vision route.',
  ],
  ['r_reviveFX_contrastCenter', 'float', 'Revive centre'],
  ['r_reviveFX_brightnessCenter', 'float', 'Revive centre'],
  ['r_reviveFX_desaturationCenter', 'float', 'Revive centre'],
  ['r_reviveFX_darkTintCenter', 'vector', 'Revive centre'],
  ['r_reviveFX_lightTintCenter', 'vector', 'Revive centre'],
  ['snd_masterRingmod', 'float', 'Audio metadata'],
  ['snd_reverbRingmod', 'float', 'Audio metadata'],
  ['snd_hiFilter', 'float', 'Audio metadata'],
  ['snd_lowFilter', 'float', 'Audio metadata'],
];
export const visionFields = definitions.map(([name, type, group, note]) =>
  Object.freeze({
    name,
    type,
    group,
    note:
      note ||
      (group === 'Audio metadata' ? 'Preserved as file data; this app does not reproduce audio.' : ''),
  }),
);
const fieldsByName = new Map(visionFields.map((field) => [field.name.toLowerCase(), field]));
const floatPrefix = /^[+-]?(?:(?:\d+(?:\.\d*)?|\.\d+)(?:[eE][+-]?\d+)?|inf(?:inity)?|nan(?:\([^)]*\))?)/i;

export function zeroVision() {
  return Object.fromEntries(
    visionFields.map((field) => [
      field.name,
      field.type === 'vector' ? [0, 0, 0] : field.type === 'bool' ? false : 0,
    ]),
  );
}

function finiteFloat(value) {
  const rounded = Math.fround(value);
  if (!Number.isFinite(value) || !Number.isFinite(rounded)) {
    throw new Error('Values must be finite and fit a 32-bit float.');
  }
  return rounded;
}

function readValue(text, type) {
  if (type === 'bool') {
    const match = text.trimStart().match(/^[+-]?(?:0[xX][0-9a-fA-F]+|0[0-7]*|[1-9][0-9]*)/);
    if (!match) return null;
    const token = match[0];
    const sign = token[0] === '-' ? -1 : 1;
    const body = token.replace(/^[+-]/, '');
    const base = /^0x/i.test(body) ? 16 : body[0] === '0' ? 8 : 10;
    return sign * parseInt(body.replace(/^0x/i, ''), base) !== 0;
  }
  const values = [];
  let remaining = text;
  const count = type === 'vector' ? 3 : 1;
  for (let index = 0; index < count; index += 1) {
    remaining = remaining.trimStart();
    const match = remaining.match(floatPrefix);
    if (!match) return null;
    values.push(Math.fround(Number(match[0])));
    remaining = remaining.slice(match[0].length);
  }
  return count === 3 ? values : values[0];
}

function sourceGuard(text) {
  if (typeof text !== 'string') return 'Vision source must be text.';
  if (text.length > 1024 * 1024) return 'Vision source exceeds the 1 MiB limit.';
  if (text.includes('\0')) return 'NUL bytes are not supported.';
  if (/[^\x01-\x7f]/.test(text)) return 'Only ASCII Vision files are supported.';
  return null;
}

// A missing value must never consume the key on the next line.
function tokenReader(text) {
  const errors = [];
  let cursor = 0;
  let line = 1;
  function next(allowLineBreak) {
    while (cursor < text.length) {
      const char = text[cursor];
      if (/\s/.test(char)) {
        if (char === '\n' && !allowLineBreak) return null;
        if (char === '\n') line += 1;
        cursor += 1;
        continue;
      }
      if (text.startsWith('//', cursor)) {
        const end = text.indexOf('\n', cursor);
        cursor = end < 0 ? text.length : end;
        continue;
      }
      if (text.startsWith('/*', cursor)) {
        const end = text.indexOf('*/', cursor + 2);
        if (end < 0) {
          errors.push(`Line ${line}: unterminated block comment.`);
          cursor = text.length;
          return null;
        }
        const previousLine = line;
        line += (text.slice(cursor, end + 2).match(/\n/g) || []).length;
        cursor = end + 2;
        if (!allowLineBreak && line !== previousLine) return null;
        continue;
      }
      const start = cursor;
      const startLine = line;
      if (char === '"') {
        cursor += 1;
        const contentStart = cursor;
        while (cursor < text.length && text[cursor] !== '"') {
          if (text[cursor] === '\n') line += 1;
          cursor += 1;
        }
        if (cursor === text.length) {
          errors.push(`Line ${startLine}: unterminated quote.`);
          return null;
        }
        const value = text.slice(contentStart, cursor);
        cursor += 1;
        return { text: value, line: startLine };
      } else {
        while (cursor < text.length && !/\s/.test(text[cursor])) cursor += 1;
        return { text: text.slice(start, cursor), line: startLine };
      }
    }
    return null;
  }
  return {
    next,
    errors,
    skipLine() {
      while (cursor < text.length && text[cursor] !== '\n') cursor += 1;
    },
    get line() {
      return line;
    },
  };
}

export function parseVision(text, baseline = zeroVision()) {
  const values = structuredClone(baseline);
  const warnings = [];
  const seen = new Set();
  const fatal = sourceGuard(text);
  if (fatal) return { values, warnings, errors: [fatal], safe: false };
  const scanned = tokenReader(text);
  const errors = scanned.errors;
  while (true) {
    const key = scanned.next(true);
    if (!key) break;
    if (key.text === '') {
      errors.push(`Line ${key.line}: an empty quoted key terminates WaW parsing. Remove it before editing.`);
      break;
    }
    const field = fieldsByName.get(key.text.toLowerCase());
    const value = scanned.next(false);
    if (!field) warnings.push(`Line ${key.line}: unknown field “${key.text}” retained.`);
    else if (seen.has(field.name))
      warnings.push(`Line ${key.line}: duplicate ${field.name} ignored; first valid entry wins.`);
    else {
      const parsed = value ? readValue(value.text, field.type) : null;
      if (parsed === null)
        warnings.push(`Line ${key.line}: invalid or missing ${field.name}; baseline retained.`);
      else {
        values[field.name] = parsed;
        seen.add(field.name);
      }
    }
    if (value || scanned.line === key.line) scanned.skipLine();
  }
  for (const field of visionFields) {
    const value = values[field.name];
    if (field.type !== 'bool' && !(Array.isArray(value) ? value : [value]).every(Number.isFinite)) {
      errors.push(`${field.name}: nonfinite or out-of-float32 source values cannot be previewed.`);
    }
  }
  return { values, warnings, errors, safe: errors.length === 0 };
}

export function installedBaseline(defaultText) {
  const parsed = parseVision(defaultText, zeroVision());
  if (!parsed.safe) throw new Error(`Installed default.vision is invalid: ${parsed.errors.join(' ')}`);
  return parsed.values;
}

export function validateVisionValue(name, value) {
  const field = fieldsByName.get(name.toLowerCase());
  if (!field) throw new Error('Unknown Vision field.');
  if (field.type === 'bool') {
    if (typeof value !== 'boolean') throw new Error(`${name} must be a boolean.`);
    return value;
  }
  if (field.type === 'vector') {
    if (!Array.isArray(value) || value.length !== 3) throw new Error(`${name} needs three RGB numbers.`);
    return value.map(finiteFloat);
  }
  return finiteFloat(value);
}

export function formatVisionValue(value) {
  if (Array.isArray(value)) return value.map(formatVisionValue).join(' ');
  if (typeof value !== 'number' || !Number.isFinite(value)) return String(value);
  // Show the shortest useful decimal that reads back to the same float32.
  // The controls should say 0.34, rather than expose binary rounding noise.
  const rounded = Math.fround(value);
  for (let digits = 1; digits <= 9; digits += 1) {
    const text = String(Number(rounded.toPrecision(digits)));
    if (Math.fround(Number(text)) === rounded) return text;
  }
  return String(rounded);
}
