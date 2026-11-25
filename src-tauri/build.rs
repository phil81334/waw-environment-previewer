use std::{env, fs, path::Path};

fn main() {
    println!("cargo:rustc-link-arg-bin=waw-environment-previewer=/EXPORT:NvOptimusEnablement");
    println!("cargo:rustc-link-arg-bin=waw-environment-previewer=/EXPORT:AmdPowerXpressRequestHighPerformance");
    embed_scene();
    tauri_build::build();
}

fn embed_scene() {
    let scenes = Path::new("../scenes");
    let selector = scenes.join("active.json");
    println!("cargo:rerun-if-changed={}", selector.display());
    let selected: serde_json::Value =
        serde_json::from_slice(&fs::read(selector).expect("read scenes/active.json"))
            .expect("parse scenes/active.json");
    let folder = selected["folder"].as_str().expect("active scene folder");
    assert!(
        !folder.is_empty()
            && folder
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'-' | b'_')),
        "scene folder must contain only letters, digits, hyphens or underscores"
    );
    let scene = scenes.join(folder);
    let output = env::var_os("OUT_DIR").expect("Cargo OUT_DIR");
    for name in ["scene.map", "profile.json"] {
        let path = scene.join(name);
        println!("cargo:rerun-if-changed={}", path.display());
        fs::copy(&path, Path::new(&output).join(name))
            .unwrap_or_else(|error| panic!("cannot embed {}: {error}", path.display()));
    }
    println!("cargo:rustc-env=WAW_SCENE_FOLDER={folder}");
}
