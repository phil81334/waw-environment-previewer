const cross = (a, b) => [a[1] * b[2] - a[2] * b[1], a[2] * b[0] - a[0] * b[2], a[0] * b[1] - a[1] * b[0]];
const add = (a, b) => a.map((value, axis) => value + b[axis]);
const subtract = (a, b) => a.map((value, axis) => value - b[axis]);
const scale = (a, amount) => a.map((value) => value * amount);

function normalize(vector) {
  const length = Math.hypot(...vector);
  return length > 1e-12 ? scale(vector, 1 / length) : [0, 0, 1];
}

function cameraVectors(state) {
  const forward = [
    Math.cos(state.pitch) * Math.cos(state.yaw),
    Math.cos(state.pitch) * Math.sin(state.yaw),
    Math.sin(state.pitch),
  ];
  let right = cross(forward, [0, 0, 1]);
  if (Math.hypot(...right) < 1e-6) {
    right = cross(forward, [0, 1, 0]);
  }
  right = normalize(right);
  return { forward, right, up: cross(right, forward) };
}

export function multiply4(a, b) {
  const result = new Float32Array(16);
  for (let column = 0; column < 4; column += 1) {
    for (let row = 0; row < 4; row += 1) {
      let value = 0;
      for (let inner = 0; inner < 4; inner += 1) {
        value += a[inner * 4 + row] * b[column * 4 + inner];
      }
      result[column * 4 + row] = value;
    }
  }
  return result;
}

// The native renderer rebases vertices to the eye before applying this matrix.
export function basisView(right, up, forward) {
  return new Float32Array([
    right[0],
    up[0],
    forward[0],
    0,
    right[1],
    up[1],
    forward[1],
    0,
    right[2],
    up[2],
    forward[2],
    0,
    0,
    0,
    0,
    1,
  ]);
}

export function cameraMatrix(camera, aspect) {
  const { right, up, forward } = cameraVectors(camera);
  // WaW uses the same far-cap projection for the sky and world geometry.
  const depthScale = 2047 / 2048;
  const focal = depthScale / Math.tan((camera.fov * Math.PI) / 360);
  const projection = new Float32Array([
    focal / aspect,
    0,
    0,
    0,
    0,
    focal,
    0,
    0,
    0,
    0,
    depthScale,
    1,
    0,
    0,
    -4 * depthScale,
    0,
  ]);
  return multiply4(projection, basisView(right, up, forward));
}

export function cameraAt(position, target) {
  const forward = normalize(subtract(target, position));
  return {
    position: [...position],
    yaw: Math.atan2(forward[1], forward[0]),
    pitch: Math.asin(forward[2]),
    fov: 75,
  };
}

export function createCameraController(canvas, onChange = () => {}) {
  let state = cameraAt([-200, -208, 106], [0, 0, 100]);
  let speed = 160;
  let rotating = false;
  const keys = new Set();
  const abort = new AbortController();
  const options = { signal: abort.signal };
  canvas.tabIndex = 0;
  function notify() {
    onChange(getState());
  }
  function getState() {
    return structuredClone(state);
  }
  canvas.addEventListener('contextmenu', (event) => event.preventDefault(), options);
  canvas.addEventListener(
    'pointerdown',
    (event) => {
      if (event.button === 2) {
        canvas.focus();
        canvas.setPointerCapture(event.pointerId);
        rotating = true;
        event.preventDefault();
      }
    },
    options,
  );
  canvas.addEventListener(
    'pointermove',
    (event) => {
      if (rotating) {
        state.yaw -= event.movementX * 0.004;
        state.pitch = Math.max(-1.55, Math.min(1.55, state.pitch - event.movementY * 0.004));
        notify();
      }
    },
    options,
  );
  const release = () => {
    rotating = false;
  };
  canvas.addEventListener('pointerup', release, options);
  canvas.addEventListener('lostpointercapture', release, options);
  canvas.addEventListener(
    'wheel',
    (event) => {
      canvas.focus();
      const { forward } = cameraVectors(state);
      state.position = add(state.position, scale(forward, -Math.sign(event.deltaY) * speed * 0.15));
      notify();
      event.preventDefault();
    },
    { ...options, passive: false },
  );
  canvas.addEventListener(
    'keydown',
    (event) => {
      if (['KeyW', 'KeyA', 'KeyS', 'KeyD', 'KeyQ', 'KeyE', 'ShiftLeft', 'ShiftRight'].includes(event.code)) {
        keys.add(event.code);
        event.preventDefault();
      }
    },
    options,
  );
  window.addEventListener('keyup', (event) => keys.delete(event.code), options);
  canvas.addEventListener('blur', () => keys.clear(), options);
  return {
    getState,
    setState(value) {
      if (
        !value ||
        !Array.isArray(value.position) ||
        value.position.length !== 3 ||
        value.position.some((x) => !Number.isFinite(x)) ||
        !Number.isFinite(value.yaw) ||
        !Number.isFinite(value.pitch)
      ) {
        throw new Error('Saved camera is invalid.');
      }
      state = {
        position: [...value.position],
        yaw: value.yaw,
        pitch: Math.max(-1.55, Math.min(1.55, value.pitch)),
        fov: 75,
      };
      notify();
    },
    update(seconds) {
      if (keys.size === 0) return;
      const { forward, right } = cameraVectors(state);
      let motion = [0, 0, 0];
      if (keys.has('KeyW')) {
        motion = add(motion, forward);
      }
      if (keys.has('KeyS')) {
        motion = subtract(motion, forward);
      }
      if (keys.has('KeyD')) {
        motion = add(motion, right);
      }
      if (keys.has('KeyA')) {
        motion = subtract(motion, right);
      }
      if (keys.has('KeyE')) {
        motion[2] += 1;
      }
      if (keys.has('KeyQ')) {
        motion[2] -= 1;
      }
      if (Math.hypot(...motion) > 0) {
        const boost = keys.has('ShiftLeft') || keys.has('ShiftRight') ? 3 : 1;
        state.position = add(
          state.position,
          scale(normalize(motion), speed * boost * Math.min(seconds, 0.1)),
        );
        notify();
      }
    },
    setSpeed(value) {
      speed = Math.max(10, Math.min(1000, value));
    },
    destroy() {
      abort.abort();
    },
  };
}
