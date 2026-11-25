// Bake settings for the fixed scene. These never modify the original map.
const worldFields = [
  ['sunlight', 'Sunlight', 1],
  ['suncolor', 'Sun colour · R G B', 3],
  ['sundirection', 'Sun direction · pitch yaw roll', 3],
  ['sunIsPrimaryLight', 'Sun is a primary light', 'primary'],
  ['ambient', 'Ambient strength', 1],
  ['_color', 'Ambient colour · R G B', 3],
  ['diffusefraction', 'Diffuse fraction', 1],
  ['sundiffusecolor', 'Sun diffuse colour · R G B', 3],
  ['sunradiosity', 'Sun radiosity', 1],
  ['radiosityscale', 'Radiosity scale', 1],
  ['contrastgain', 'Contrast gain', 1],
  ['northyaw', 'North yaw', 1],
  ['treeScatterIntensity', 'Tree scatter intensity', 1],
  ['treeScatterAmount', 'Tree scatter amount', 1],
  ['skyboxmodel', 'Skybox model', 'sky'],
];
const clone = (value) => structuredClone(value);

export function createEnvironmentControls({ onChange = () => {}, onFocus = () => {} } = {}) {
  const byId = (id) => document.getElementById(id);
  const worldInputs = new Map();
  let defaults = null;
  let state = null;
  let selectedIndex = null;
  let busy = false;
  let baked = null;
  let skyModels = [];

  function selected() {
    return state?.lights.find((light) => light.index === selectedIndex) ?? null;
  }
  function currentProperty(key) {
    return state?.worldspawn?.properties.find((pair) => pair[0] === key)?.[1];
  }
  function setProperty(key, value) {
    const properties = state.worldspawn.properties;
    const index = properties.findIndex((pair) => pair[0] === key);
    // Empty means use the original compiler default (omit the key).
    if (value === '' && key !== 'skyboxmodel') {
      if (index >= 0) properties.splice(index, 1);
    } else if (index >= 0) properties[index] = [key, value];
    else properties.push([key, value]);
  }
  function validate(candidate) {
    if (
      !candidate?.worldspawn ||
      !Array.isArray(candidate.worldspawn.properties) ||
      !Array.isArray(candidate.lights)
    ) {
      throw new Error('Environment settings are unavailable.');
    }
    for (const light of candidate.lights) {
      if (
        !Array.isArray(light.color) ||
        light.color.length !== 3 ||
        light.color.some((v) => !Number.isFinite(v) || v < 0 || v > 1) ||
        !light.color.some((v) => v > 0)
      ) {
        throw new Error(
          'Light colour needs three values from 0 to 1, with at least one above zero. Use intensity 0 to switch off.',
        );
      }
      if (!Number.isFinite(light.intensity) || light.intensity < 0 || light.intensity > 4)
        throw new Error('Light intensity must be from 0 to 4.');
    }
    for (const [key, , kind] of worldFields) {
      const value = candidate.worldspawn.properties.find((pair) => pair[0] === key)?.[1];
      if (value === undefined || value === '') continue;
      if (kind === 'sky') {
        if (!skyModels.includes(value)) throw new Error('Choose one of the supported installed sky models.');
        continue;
      }
      const numbers = value.trim().split(/\s+/).map(Number);
      const count = kind === 'primary' ? 1 : kind;
      if (numbers.length !== count || numbers.some((v) => !Number.isFinite(v) || Math.abs(v) > 36000))
        throw new Error(`${key} needs ${count} finite numeric value(s).`);
      if (kind === 'primary' && ![0, 1].includes(numbers[0]))
        throw new Error('Sun primary-light setting must be 0 or 1.');
      if (['suncolor', 'sundiffusecolor', '_color'].includes(key) && numbers.some((v) => v < 0 || v > 1))
        throw new Error(`${key} components must be from 0 to 1.`);
      if (
        [
          'sunlight',
          'ambient',
          'diffusefraction',
          'radiosityscale',
          'contrastgain',
          'treeScatterIntensity',
          'treeScatterAmount',
        ].includes(key) &&
        (numbers[0] < 0 || numbers[0] > 16)
      )
        throw new Error(`${key} must be from 0 to 16.`);
      if (key === 'sunradiosity' && Math.abs(numbers[0]) > 16)
        throw new Error('Sun radiosity must be from −16 to 16.');
    }
  }
  function isDirty() {
    return Boolean(state && JSON.stringify(state) !== baked);
  }
  function status(error = '') {
    const element = byId('environment-edit-status');
    element.textContent =
      error ||
      (!state
        ? 'Lighting settings load at startup.'
        : isDirty()
          ? 'Lighting / worldspawn settings need Bake. The last completed scene stays visible.'
          : 'Lighting / worldspawn settings match the current bake.');
    element.className = error ? 'hint error' : 'hint';
  }
  function change(edit, errorElement = 'environment-edit-status') {
    const previous = clone(state);
    try {
      edit();
      validate(state);
    } catch (error) {
      // Invalid text must not poison later session saves or bakes.
      state = previous;
      renderLight();
      renderWorld();
      const feedback = byId(errorElement);
      feedback.textContent = error.message;
      feedback.className = 'hint error';
      return;
    }
    if (errorElement === 'environment-light-position') renderLight();
    status();
    onChange();
  }
  function renderLight() {
    const light = selected();
    ['red', 'green', 'blue'].forEach((component, index) => {
      byId(`environment-light-${component}`).value = light?.color[index] ?? '';
    });
    byId('environment-light-intensity').value = light?.intensity ?? '';
    const authored = defaults?.lights.find((item) => item.index === selectedIndex);
    byId('environment-light-position').className = 'hint';
    byId('environment-light-position').textContent = authored
      ? `${authored.label || `Light ${selectedIndex + 1}`} · origin ${authored.origin.join(' ')}`
      : 'No authored lights in this scene.';
    for (const id of [
      'environment-light-list',
      'environment-light-red',
      'environment-light-green',
      'environment-light-blue',
      'environment-light-intensity',
      'environment-reset-light',
      'environment-view-light',
      'environment-reset-lights',
    ]) {
      byId(id).disabled = busy || !light;
    }
  }
  function renderWorld() {
    for (const [key, input] of worldInputs) {
      input.value = currentProperty(key) ?? '';
      input.disabled = busy || !state;
    }
    byId('environment-reset-world').disabled = busy || !state;
  }
  function createWorldFields() {
    const parent = byId('environment-world-fields');
    parent.replaceChildren();
    worldInputs.clear();
    for (const [key, title, kind] of worldFields) {
      const label = document.createElement('label');
      label.className = 'field';
      label.append(document.createTextNode(title));
      let input;
      if (kind === 'sky' || kind === 'primary') {
        input = document.createElement('select');
        if (kind === 'sky') {
          input.add(new Option('No sky model', ''));
          for (const name of skyModels) input.add(new Option(name, name));
        } else {
          input.add(new Option('Compiler default (omitted)', ''));
          input.add(new Option('No', '0'));
          input.add(new Option('Yes', '1'));
        }
      } else {
        input = document.createElement('input');
        input.type = 'text';
        input.inputMode = 'decimal';
        input.spellcheck = false;
        input.placeholder = 'Compiler default (omitted)';
      }
      input.id = `environment-world-${key}`;
      input.onchange = () => {
        if (!state || busy) return;
        change(() => setProperty(key, input.value.trim()));
      };
      label.append(input);
      parent.append(label);
      worldInputs.set(key, input);
    }
  }
  byId('environment-light-list').onchange = () => {
    selectedIndex = Number(byId('environment-light-list').value);
    renderLight();
  };
  ['red', 'green', 'blue'].forEach((component, index) => {
    byId(`environment-light-${component}`).onchange = (event) => {
      const light = selected();
      if (!light || busy) return;
      change(() => {
        light.color[index] = event.target.value.trim() === '' ? NaN : Number(event.target.value);
      }, 'environment-light-position');
    };
  });
  byId('environment-light-intensity').onchange = (event) => {
    const light = selected();
    if (!light || busy) return;
    change(() => {
      light.intensity = event.target.value.trim() === '' ? NaN : Number(event.target.value);
    }, 'environment-light-position');
  };
  byId('environment-reset-light').onclick = () => {
    const index = state?.lights.findIndex((light) => light.index === selectedIndex) ?? -1;
    if (index < 0 || busy) return;
    const authored = defaults.lights.find((light) => light.index === selectedIndex);
    change(() => {
      state.lights[index] = {
        index: authored.index,
        color: clone(authored.color),
        intensity: authored.intensity,
      };
    });
    renderLight();
  };
  function resetLights() {
    if (!state || busy) return;
    change(() => {
      state.lights = defaults.lights.map(({ index, color, intensity }) => ({
        index,
        color: clone(color),
        intensity,
      }));
    });
    renderLight();
  }
  function resetWorld() {
    if (!state || busy) return;
    change(() => {
      state.worldspawn = clone(defaults.worldspawn);
    });
    renderWorld();
  }
  function selectedLight() {
    const authored = defaults?.lights.find((light) => light.index === selectedIndex);
    return authored ? clone(authored) : null;
  }
  byId('environment-reset-world').onclick = resetWorld;
  // Camera navigation does not dirty lighting inputs or require a bake.
  byId('environment-view-light').onclick = () => {
    const light = selectedLight();
    if (light && !busy) onFocus(light);
  };
  byId('environment-reset-lights').onclick = resetLights;
  return {
    initialize(metadata) {
      defaults = clone(metadata.environmentDefaults);
      if (!defaults?.worldspawn || !Array.isArray(defaults.lights))
        throw new Error('Installed scene environment defaults are unavailable.');
      const installedSkies = new Map((metadata.skyModels || []).map((name) => [name.toLowerCase(), name]));
      const authoredSky = defaults.worldspawn.properties.find((pair) => pair[0] === 'skyboxmodel')?.[1];
      if (authoredSky) installedSkies.set(authoredSky.toLowerCase(), authoredSky);
      skyModels = [...installedSkies.values()].sort((a, b) => a.localeCompare(b));
      state = {
        worldspawn: clone(defaults.worldspawn),
        lights: defaults.lights.map(({ index, color, intensity }) => ({
          index,
          color: clone(color),
          intensity,
        })),
      };
      selectedIndex = state.lights[0]?.index ?? null;
      byId('environment-light-list').replaceChildren(
        ...defaults.lights.map(
          (light) => new Option(light.label || `Light ${light.index + 1}`, String(light.index)),
        ),
      );
      createWorldFields();
      renderLight();
      renderWorld();
      status();
    },
    snapshot() {
      validate(state);
      return clone(state);
    },
    restore(candidate) {
      validate(candidate);
      const expected = defaults.lights.map((light) => light.index);
      if (
        candidate.lights.length !== expected.length ||
        candidate.lights.some((light, i) => light.index !== expected[i])
      )
        throw new Error('Saved environment lights do not match this scene.');
      if (
        candidate.worldspawn.scene !== defaults.worldspawn.scene ||
        candidate.worldspawn.id !== defaults.worldspawn.id
      )
        throw new Error('Saved worldspawn does not match this scene.');
      state = clone(candidate);
      renderLight();
      renderWorld();
      status();
    },
    setBusy(value) {
      if (busy === Boolean(value)) return;
      busy = Boolean(value);
      renderLight();
      renderWorld();
    },
    markBaked(snapshot = state) {
      baked = JSON.stringify(snapshot);
      status();
    },
    isDirty,
    selectedLight,
    resetLights,
    resetWorld,
  };
}
