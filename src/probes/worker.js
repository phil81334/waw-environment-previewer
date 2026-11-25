import { filterCube } from './processing.js';

self.onmessage = ({ data }) => {
  try {
    const cubes = data.frames.map((frames, probe) =>
      filterCube(frames, (message) => self.postMessage({ message: `Probe ${probe + 1}: ${message}` })),
    );
    const transfers = cubes.flat(2).map((face) => face.buffer);
    self.postMessage({ cubes }, transfers);
  } catch (error) {
    self.postMessage({ error: error.message || String(error) });
  }
};
