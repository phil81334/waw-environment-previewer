// Float32 operation order follows the original WaW post-effect setup/planner.
// This module has no dependency on author research or game executable code.
const f = Math.fround;
const compactRadius = f(1.3895604610443115);
const minimumRadius = f(0.3295051157474518);
const axisRadiusLimit = f(6.497750282287598);
const axisVarianceLimit = f(42.220760345458984);

export function gradeConstants({
  contrast = 1,
  brightness = 0,
  desaturation = 0,
  invert = false,
  dark = [1, 1, 1],
  light = [1, 1, 1],
} = {}) {
  const amount = f(Math.max(f(desaturation), 1 / 4096));
  let scale = f(f(contrast) * amount);
  let bias = f(f(f(brightness) + f(0.5)) - f(f(contrast) * f(0.5)));
  if (invert) {
    scale = f(-scale);
    bias = f(bias + 1);
  }
  const result = new Float32Array([
    ...dark.map((value) => f(f(value) * scale)),
    0,
    ...light.map((value, index) => f(f(f(value) - f(dark[index])) * scale)),
    0,
    bias,
    bias,
    bias,
    f(f(1 / amount) - 1),
  ]);
  if (!result.every(Number.isFinite)) {
    throw new Error('These colour values exceed the supported float32 preview range.');
  }
  return result;
}

export function filmParameters(values) {
  // Disabling film also selects neutral extraction-grade constants in the
  // original frontend, even when stored film fields are non-neutral.
  if (!values.r_filmEnable) return {};
  return {
    contrast: values.r_filmContrast,
    brightness: values.r_filmBrightness,
    desaturation: values.r_filmDesaturation,
    invert: values.r_filmInvert,
    dark: values.r_filmDarkTint,
    light: values.r_filmLightTint,
  };
}

export function reviveParameters(values, suffix) {
  return {
    contrast: values[`r_reviveFX_contrast${suffix}`],
    brightness: values[`r_reviveFX_brightness${suffix}`],
    desaturation: values[`r_reviveFX_desaturation${suffix}`],
    dark: values[`r_reviveFX_darkTint${suffix}`],
    light: values[`r_reviveFX_lightTint${suffix}`],
  };
}

export function filmPassEnabled(values) {
  return Boolean(
    values.r_filmEnable &&
    (values.r_filmContrast !== 1 ||
      values.r_filmBrightness !== 0 ||
      values.r_filmDesaturation !== 0 ||
      values.r_filmInvert ||
      values.r_filmDarkTint.some((value) => value !== 1) ||
      values.r_filmLightTint.some((value) => value !== 1)),
  );
}

export function glowPassEnabled(values) {
  // Original normal gate requires both nonzero intensity0 and radius0.
  // Rays do not independently activate a disabled bloom pass.
  return Boolean(values.r_glow && values.r_glowBloomIntensity0 !== 0 && values.r_glowRadius0 !== 0);
}

function gaussianPairs(radius, sourceSize, targetSize, maximum) {
  const phase = Math.round(f(f(sourceSize) / f(targetSize)) + 2 ** -30) & 1 ? 0 : f(0.5);
  const exponent = f(f(-0.5) / f(radius * radius));
  const offsets = [];
  const weights = [];
  let total = f(0);
  for (let index = 0; index < maximum; index += 1) {
    const a = f(f(2 * index) + phase);
    const b = f(f(2 * index + 1) + phase);
    let first = f(Math.exp(f(f(a * a) * exponent)));
    const second = f(Math.exp(f(f(b * b) * exponent)));
    if (index === 0 && phase === 0) {
      first = f(first * f(0.5));
    }
    const weight = f(first + second);
    const offset =
      weight === 0
        ? f(f(f(a + b) * f(0.5)) / f(sourceSize))
        : f(f(f(second * b) + f(first * a)) / f(weight * f(sourceSize)));
    offsets.push(offset);
    weights.push(weight);
    total = f(total + weight);
  }
  if (total <= f(0.001)) {
    weights[0] = f(0.5);
    return { count: 1, offsets, weights };
  }
  const normalization = f(f(0.5) / total);
  let count = maximum;
  for (let index = maximum - 1; index >= 0; index -= 1) {
    weights[index] = f(weights[index] * normalization);
    if (weights[index] < f(0.01)) {
      count = index + 1;
    }
  }
  return { count, offsets, weights };
}

function compactPass(radius, source, target) {
  const horizontal = gaussianPairs(radius, source[0], target[0], 2);
  const vertical = gaussianPairs(radius, source[1], target[1], 2);
  const taps = [];
  for (let y = 0; y < 2; y += 1) {
    for (let x = 0; x < 2; x += 1) {
      const weight = f(horizontal.weights[x] * vertical.weights[y]);
      taps.push(f(-0 - horizontal.offsets[x]), vertical.offsets[y], 0, weight);
      taps.push(horizontal.offsets[x], vertical.offsets[y], 0, weight);
    }
  }
  return { count: 8, taps: new Float32Array(taps) };
}

function axisPass(radius, target, axis) {
  const pairs = gaussianPairs(radius, target[axis], target[axis], 8);
  const taps = [];
  for (let index = 0; index < 8; index += 1) {
    taps.push(
      axis === 0 ? pairs.offsets[index] : 0,
      axis === 1 ? pairs.offsets[index] : 0,
      0,
      pairs.weights[index],
    );
  }
  return { count: pairs.count, taps: new Float32Array(taps) };
}

export function blurPlan(radii, source, target) {
  let [horizontal, vertical] = radii.map(f);
  if (
    ![horizontal, vertical].every(Number.isFinite) ||
    !Number.isFinite(f(horizontal * horizontal)) ||
    !Number.isFinite(f(vertical * vertical))
  ) {
    throw new Error('Blur radius exceeds the supported float32 preview range.');
  }
  const result = [];
  if (source[0] !== target[0] || source[1] !== target[1]) {
    const sigma = f(Math.min(horizontal, vertical, compactRadius));
    const variance = f(sigma * sigma);
    result.push(compactPass(sigma, source, target));
    horizontal = f(
      f(f(Math.sqrt(Math.abs(f(f(horizontal * horizontal) - variance)))) * f(target[0])) / f(source[0]),
    );
    vertical = f(
      f(f(Math.sqrt(Math.abs(f(f(vertical * vertical) - variance)))) * f(target[1])) / f(source[1]),
    );
  }
  while (result.length < 32 && (horizontal >= minimumRadius || vertical >= minimumRadius)) {
    if (f(Math.abs(f(horizontal - vertical))) < minimumRadius) {
      const average = f(f(horizontal + vertical) * f(0.5));
      if (average <= compactRadius) {
        result.push(compactPass(average, target, target));
        break;
      }
    }
    const axis = horizontal > vertical ? 0 : 1;
    let sigma = axis === 0 ? horizontal : vertical;
    let remaining = f(0);
    if ((axis === 0 && sigma >= axisRadiusLimit) || (axis === 1 && sigma > axisRadiusLimit)) {
      remaining = f(Math.sqrt(f(f(sigma * sigma) - axisVarianceLimit)));
      sigma = axisRadiusLimit;
    }
    if (axis === 0) {
      horizontal = remaining;
    } else {
      vertical = remaining;
    }
    result.push(axisPass(sigma, target, axis));
  }
  for (const pass of result) {
    if (!pass.taps.every(Number.isFinite)) {
      throw new Error('The blur configuration generated unsupported nonfinite filter coefficients.');
    }
  }
  return result;
}

export function rayProjection(camera, sun, aspect) {
  if (!sun || sun.length !== 3 || !sun.every(Number.isFinite)) {
    return { centre: [0.5, 0.5], visibility: 0 };
  }
  const cp = f(Math.cos(camera.pitch));
  const sp = f(Math.sin(camera.pitch));
  const cy = f(Math.cos(camera.yaw));
  const sy = f(Math.sin(camera.yaw));
  const forward = [f(cp * cy), f(cp * sy), sp];
  const left = [f(-sy), cy, 0];
  const up = [f(-f(sp * cy)), f(-f(sp * sy)), cp];
  const vector = sun.map(f);
  const depth = f(f(f(forward[1] * vector[1]) + f(forward[2] * vector[2])) + f(forward[0] * vector[0]));
  if (depth <= 0) {
    return { centre: [0.5, 0.5], visibility: 0 };
  }
  const lateral = f(f(f(left[0] * vector[0]) + f(left[1] * vector[1])) + f(left[2] * vector[2]));
  const height = f(f(f(up[0] * vector[0]) + f(up[1] * vector[1])) + f(up[2] * vector[2]));
  const tangentY = f(Math.tan(((camera.fov ?? 75) * Math.PI) / 360));
  const tangentX = f(f(aspect) * tangentY);
  const inverse = f(1 / depth);
  const x = f(f(f(inverse * lateral) / tangentX) * f(-0.5));
  const y = f(f(f(inverse * height) / tangentY) * f(-0.5));
  return {
    centre: [f(f(x + 1) * f(0.5)), f(f(y + 1) * f(0.5))],
    visibility: depth >= 1 ? 1 : depth < f(0.3) ? 0 : f(f(depth - f(0.3)) / f(1 - f(0.3))),
  };
}
