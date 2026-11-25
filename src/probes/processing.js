// CPU portions of the checked WaW first-generation capture profile. Explicit
// float32 operations preserve the original degenerate CSV branches as well.
const f = Math.fround;
const inv255 = f(1 / 255);

export function resolveCorrection(name, rows) {
  if (!rows?.length) throw new Error('No reflection correction table.');
  const requested = name.trimEnd() || 'default';
  const row = rows.find((row) => row.name.toLowerCase() === requested.toLowerCase());
  return {
    row: row ?? rows[0],
    warning: row ? '' : `Unknown correction “${requested}”; using first CSV row “${rows[0].name}”.`,
  };
}

export function correctCube(cube, row) {
  const inverseRange = f(1 / f(row.white - row.black));
  return cube.map((faces) =>
    faces.map((bytes) => {
      const output = new Uint8Array(bytes.length);
      for (let i = 0; i < bytes.length; i += 4) {
        const channels = [0, 1, 2].map((c) => {
          let x = f(f(f(bytes[i + c] * inv255) - f(row.black)) * inverseRange);
          if (!(x >= 0)) x = 0;
          return f(Math.pow(x, f(row.gamma)));
        });
        const gray = f(f(f(channels[1] * f(0.587)) + f(channels[0] * f(0.299))) + f(channels[2] * f(0.114)));
        const values = channels.map((channel) => {
          let value = f(f(f(1 - f(row.saturation)) * gray) + f(channel * f(row.saturation)));
          if (!(f(4 - value) >= 0)) value = 4;
          return value;
        });
        let scale = f(0.1);
        for (const c of [2, 1, 0]) if (values[c] > scale) scale = values[c];
        const reciprocal = f(1 / scale);
        for (let c = 0; c < 3; c += 1) {
          const x = f(values[c] * reciprocal);
          const upper = Number.isNaN(x) || f(x - 1) < 0 ? x : 1;
          const result = f(-0 - x) >= 0 ? 0 : upper;
          output[i + c] = Math.trunc(f(result * 255));
        }
        output[i + 3] = Math.trunc(f(scale * 63.75));
      }
      return output;
    }),
  );
}

export function gammaTable() {
  const exponent = f(1 / f(0.9));
  return Uint8Array.from({ length: 256 }, (_, byte) => {
    const scaled = f(f(Math.pow(f(byte * inv255), exponent)) * 65535);
    return Math.trunc((Math.round(scaled + 2 ** -30) * 255) / 65535);
  });
}

export function reduceFaces(frames) {
  if (
    frames.length !== 6 ||
    frames.some((frame) => !(frame instanceof Uint8Array) || frame.length !== 258 * 258 * 4)
  ) {
    throw new Error('Capture needs six complete 258 × 258 RGBA faces.');
  }
  const gamma = gammaTable();
  return frames.map((frame, face) => {
    const output = new Uint8Array(64 * 64 * 4);
    for (let y = 0; y < 64; y += 1) {
      for (let x = 0; x < 64; x += 1) {
        let sx = [1, 3].includes(face) ? 63 - x : x;
        let sy = [1, 2].includes(face) ? 63 - y : y;
        if ([0, 1, 4, 5].includes(face)) [sx, sy] = [sy, sx];
        for (let channel = 0; channel < 4; channel += 1) {
          let sum = 0;
          for (let dy = 0; dy < 4; dy += 1) {
            for (let dx = 0; dx < 4; dx += 1) {
              const byte = frame[((1 + sy * 4 + dy) * 258 + 1 + sx * 4 + dx) * 4 + channel];
              sum += channel === 3 ? byte : gamma[byte];
            }
          }
          output[(y * 64 + x) * 4 + channel] = Math.floor(sum / 16);
        }
      }
    }
    return output;
  });
}

function integerDirection(face, x, y, k) {
  return [
    [k, k - y, k - x],
    [0, k - y, x],
    [x, k, y],
    [x, 0, k - y],
    [x, k - y, k],
    [k - x, k - y, 0],
  ][face];
}

function direction(face, x, y, side) {
  if (side === 1)
    return [
      [1, 0, 0],
      [-1, 0, 0],
      [0, 1, 0],
      [0, -1, 0],
      [0, 0, 1],
      [0, 0, -1],
    ][face];
  const k = side - 1;
  const vector = integerDirection(face, x, y, k).map((n) => f(f(n * f(1 / k)) * 2 - 1));
  const length2 = f(f(f(vector[0] * vector[0]) + f(vector[1] * vector[1])) + f(vector[2] * vector[2]));
  const reciprocal = f(1 / f(Math.sqrt(length2)));
  return vector.map((n) => f(n * reciprocal));
}

export function stitch(faces, side) {
  if (side === 1) {
    for (let c = 0; c < 4; c += 1) {
      let sum = 0;
      for (const face of faces) sum = f(sum + face[c]);
      const value = Math.trunc(f(sum * f(1 / 6)));
      for (const face of faces) face[c] = value;
    }
    return;
  }
  const groups = new Map();
  for (let face = 0; face < 6; face += 1) {
    for (let y = 0; y < side; y += 1) {
      for (let x = 0; x < side; x += 1) {
        if (x !== 0 && x !== side - 1 && y !== 0 && y !== side - 1) continue;
        const key = integerDirection(face, x, y, side - 1).join(',');
        if (!groups.has(key)) groups.set(key, []);
        groups.get(key).push([face, (y * side + x) * 4]);
      }
    }
  }
  for (const group of groups.values()) {
    for (let c = 0; c < 4; c += 1) {
      let sum = 0;
      for (const [face, offset] of group) sum = f(sum + faces[face][offset + c]);
      const value = group.length === 2 ? Math.floor(sum / 2) : Math.trunc(f(sum * f(1 / 3)));
      for (const [face, offset] of group) faces[face][offset + c] = value;
    }
  }
}

// Kernels retain the original source traversal order. Reusing them avoids
// repeating the expensive directional search for the second probe.
let kernels = null;
function buildKernels(progress) {
  const source = [];
  for (let face = 0; face < 6; face += 1) {
    for (let y = 0; y < 64; y += 1) {
      for (let x = 0; x < 64; x += 1) source.push(direction(face, x, y, 64));
    }
  }
  const angles = [
    0.0872664675116539, 0.19198621809482574, 0.3839724361896515, 0.7853981852531433, 1.5707963705062866,
    3.1415927410125732,
  ];
  return angles.map((angle, mip) => {
    progress(`Preparing original cone filter · mip ${mip + 1}/6`);
    const side = 32 >> mip;
    const threshold = f(Math.cos(angle));
    const result = [];
    for (let face = 0; face < 6; face += 1) {
      for (let y = 0; y < side; y += 1) {
        for (let x = 0; x < side; x += 1) {
          const target = direction(face, x, y, side);
          const offsets = [],
            weights = [];
          let total = 0;
          for (let s = 0; s < source.length; s += 1) {
            const v = source[s];
            const dot = f(f(f(v[0] * target[0]) + f(v[2] * target[2])) + f(v[1] * target[1]));
            const weight = f(dot - threshold);
            if (weight > 0) {
              offsets.push(s * 4);
              weights.push(weight);
              total = f(total + weight);
            }
          }
          if (!(total > 0)) throw new Error('Empty directional cone kernel.');
          result.push({
            offsets: Uint32Array.from(offsets),
            weights: Float32Array.from(weights),
            reciprocal: f(1 / total),
          });
        }
      }
    }
    return result;
  });
}

export function filterCube(frames, progress = () => {}) {
  const base = reduceFaces(frames);
  if (!kernels) kernels = buildKernels(progress);
  const all = new Uint8Array(6 * 64 * 64 * 4);
  base.forEach((face, i) => all.set(face, i * 64 * 64 * 4));
  const cube = [base.map((face) => face.slice())];
  for (let mip = 1; mip <= 6; mip += 1) {
    progress(`Filtering reflection · mip ${mip}/6`);
    const side = 64 >> mip;
    const faces = Array.from({ length: 6 }, () => new Uint8Array(side * side * 4));
    kernels[mip - 1].forEach((kernel, pixel) => {
      const face = Math.floor(pixel / (side * side));
      const offset = (pixel % (side * side)) * 4;
      for (let c = 0; c < 4; c += 1) {
        let sum = 0;
        for (let i = 0; i < kernel.offsets.length; i += 1) {
          sum = f(sum + f(f(all[kernel.offsets[i] + c] * inv255) * kernel.weights[i]));
        }
        const normalized = f(sum * kernel.reciprocal);
        faces[face][offset + c] = Math.trunc(f(Math.min(1, normalized) * 255));
      }
    });
    cube.push(faces);
  }
  cube.forEach((faces, mip) => stitch(faces, 64 >> mip));
  return cube;
}
