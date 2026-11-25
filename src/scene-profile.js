import { cameraAt } from './camera.js';
import { validateFogSettings } from './fog/state.js';
import profile from '@active-scene';

// Vite and the native build both follow scenes/active.json.
export const sceneProfile = Object.freeze({
  id: profile.id,
  title: profile.title,
  credit: profile.credit,
  defaultVision: profile.defaultVision,
  camera: cameraAt(profile.camera.eye, profile.camera.target),
  probeSettings: {
    id: `environment-${profile.id}`,
    scene: profile.id,
    globalCorrection: profile.globalCorrection,
    probes: profile.probes,
  },
  fog: validateFogSettings(profile.fog),
  fogPresets: [
    { id: 'reference', label: 'Scene reference', settings: validateFogSettings(profile.fog) },
    ...(profile.fogPresets ?? []).map(({ id, label, settings }) => ({
      id,
      label,
      settings: validateFogSettings({ ...profile.fog, ...settings }),
    })),
  ],
});
