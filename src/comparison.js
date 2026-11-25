// WaW's unzoomed cg_fov is horizontal at a 4:3 reference aspect. Its original
// projection builds tanHalfFovY = tan(cg_fov / 2) * 0.75 before applying aspect.
export const comparisonDefaults = Object.freeze({ cgFov: 65, width: 1366, height: 768, mode: 'game' });

export function validateComparison(candidate) {
  if (!candidate || !['game', 'fill'].includes(candidate.mode)) {
    throw new Error('Choose game framing or fill viewport.');
  }
  if (!Number.isFinite(candidate.cgFov) || candidate.cgFov < 10 || candidate.cgFov > 160) {
    throw new Error('Camera FOV must be between 10 and 160 degrees.');
  }
  if (
    !Number.isInteger(candidate.width) ||
    !Number.isInteger(candidate.height) ||
    candidate.width < 320 ||
    candidate.width > 3840 ||
    candidate.height < 200 ||
    candidate.height > 2160
  ) {
    throw new Error('Use a whole-number game width from 320 to 3840 and height from 200 to 2160.');
  }
  return { cgFov: candidate.cgFov, width: candidate.width, height: candidate.height, mode: candidate.mode };
}

export function comparisonAngles(settings) {
  const value = validateComparison(settings);
  const tangentY = Math.tan((value.cgFov * Math.PI) / 360) * 0.75;
  return {
    vertical: (Math.atan(tangentY) * 360) / Math.PI,
    horizontal: (Math.atan((tangentY * value.width) / value.height) * 360) / Math.PI,
  };
}

export function fitComparison(settings, width, height) {
  if (width <= 0 || height <= 0) return { width: 0, height: 0 };
  if (settings.mode === 'fill') return { width, height };
  const scale = Math.min(width / settings.width, height / settings.height);
  return { width: settings.width * scale, height: settings.height * scale };
}
