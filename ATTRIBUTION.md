# Attribution

Created by **phil81334**.

- The bundled MAP comes from **Sniperbolt’s Nazi_Zombie_Tutorial Mapping Tutorial v2.0**. Its README credits Sniperbolt for the tutorial and Sparks for the scripts that informed the initial mystery-box effect. The MAP retains its original authorship. The application's GPL applies to the previewer code.
- The application code is licensed under [GPL-3.0-only](LICENCE).
- The model readers are adapted from Phil's WaW Model Viewer. The native FX parser, simulation and shader support are adapted from his GPL-3.0-only WaW FX Viewer.
- EFX parsing and playback algorithms are adapted from xoxor4d and contributors at revision `665347c2e90671068088ddc4cd58291194cf9423`. The full MIT notice follows below. [iw3xo-radiant](https://github.com/xoxor4d/iw3xo-radiant)
- The reused FX asset readers also retain [OpenAssetTools](https://github.com/Laupetin/OpenAssetTools) GPL-3.0 attribution to Laupetin and contributors (revision `30d2d498bd8e3f2401037b24f3db6f96d39554c4`), through WaW Image Viewer, and [cod-asset-importer](https://github.com/mauserzjeh/cod-asset-importer) GPL-3.0 attribution to mauserzjeh and contributors (revision `b67ceebcc73036179f1da37aecdc0fbba6cb17bb`), through WaW Model Viewer.
- [Tauri](https://tauri.app/): MIT / Apache-2.0.
- [Vite](https://vite.dev/): MIT.
- Installed World at War and Modtools provide the user's assets, original compiler tools and shader evidence. These are not distributed with the app.
- The native renderer executes supported original installed shader programs. The backdrop provides scene lighting and reflections; effects share its camera/depth and supported fog/Vision composition. Particle illumination retains fixed packed editor-preview lighting (`0x4210`), and FX models retain the original unlit path. FX-generated light contributions to world surfaces and placed models are unsupported and are not applied.
- This is a visual preview without gameplay/GSC execution. No stock effects, shader programs, textures, models or game executables are redistributed; they are read from the user's installation.
- Shared cube/information icons and visual language follow the app family. Dependency versions are recorded in lockfiles.
- Native Direct3D 9 API through the MIT/Apache-2.0 `windows` Rust bindings. Supported shader bytecode is read from the user's installed WaW files; proprietary shader programs are not embedded or distributed. Final presentation uses the D3D9 fixed-function path.

## iw3xo-radiant upstream MIT notice

```text
MIT License
Copyright (c) 2020-2023 xoxor4d <xoxor4d@outlook.com>

https://github.com/Nukem9/detours
Copyright (c) 2019 Nukem <Nukem@outlook.com>

https://github.com/ocornut/imgui
Copyright (c) 2014-2022 Omar Cornut

https://github.com/nlohmann/fifo_map
Copyright (c) 2015-2017 Niels Lohmann

https://gist.github.com/maluoi/ade07688e741ab188841223b8ffeed22
Copyright (c) 2019 Nick Klingensmith (@koujaku)

https://github.com/ocornut/imgui/issues/1901
Copyright (c) 2021 zfedoran

https://github.com/patrickcjk/imgui-notify
Copyright (c) 2021 Patrick

Permission is hereby granted, free of charge, to any person obtaining a copy
of this software and associated documentation files (the "Software"), to deal
in the Software without restriction, including without limitation the rights
to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
copies of the Software, and to permit persons to whom the Software is
furnished to do so, subject to the following conditions:

The above copyright notice and this permission notice shall be included in all
copies or substantial portions of the Software.

THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE
SOFTWARE.
```
