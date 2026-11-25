# Credits

Created by **phil81334**. This unofficial community app is independent of Treyarch, Activision and UGX.

- The bundled MAP comes from **Sniperbolt’s Nazi_Zombie_Tutorial Mapping Tutorial v2.0**. Its README credits Sniperbolt for the tutorial and Sparks for the scripts that informed the initial mystery-box effect. The MAP retains its original authorship. The application's GPL applies to the previewer code.
- The application code is licensed under [GPL-3.0-only](LICENCE).
- The model readers are adapted from Phil's WaW Model Viewer. The native FX parser, simulation and shader support are adapted from his GPL-3.0-only WaW FX Viewer.
- EFX parsing and playback algorithms retain the [iw3xo-radiant](https://github.com/xoxor4d/iw3xo-radiant) MIT attribution to xoxor4d and contributors (revision `665347c2e90671068088ddc4cd58291194cf9423`). The complete upstream notice is included in [FX third-party notices](FX-THIRD-PARTY-NOTICES.md).
- The reused FX asset readers also retain [OpenAssetTools](https://github.com/Laupetin/OpenAssetTools) GPL-3.0 attribution to Laupetin and contributors (revision `30d2d498bd8e3f2401037b24f3db6f96d39554c4`), through WaW Image Viewer, and [cod-asset-importer](https://github.com/mauserzjeh/cod-asset-importer) GPL-3.0 attribution to mauserzjeh and contributors (revision `b67ceebcc73036179f1da37aecdc0fbba6cb17bb`), through WaW Model Viewer.
- [Tauri](https://tauri.app/): MIT / Apache-2.0.
- [Vite](https://vite.dev/): MIT.
- Installed World at War and Modtools provide the user's assets, original compiler tools and shader evidence. These are not distributed with the app.
- The native renderer executes supported original installed shader programs. The backdrop provides scene lighting and reflections; effects share its camera/depth and supported fog/Vision composition. Particle illumination retains fixed packed editor-preview lighting (`0x4210`), and FX models retain the original unlit path. FX-generated light contributions to world surfaces and placed models are unsupported and are not applied.
- This is a visual preview without gameplay/GSC execution. No stock effects, shader programs, textures, models or game executables are redistributed; they are read from the user's installation.
- Shared cube/information icons and visual language follow the app family. Dependency versions are recorded in lockfiles.
- Native Direct3D 9 API through the MIT/Apache-2.0 `windows` Rust bindings. Supported shader bytecode is read from the user's installed WaW files; proprietary shader programs are not embedded or distributed. Final presentation uses the D3D9 fixed-function path.
