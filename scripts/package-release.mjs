import { execFileSync } from "node:child_process";
import { createHash } from "node:crypto";
import { copyFileSync, mkdirSync, readFileSync, writeFileSync } from "node:fs";
import { join } from "node:path";

// List build inputs explicitly so local game paths, bake outputs and previous
// releases stay out of the source download, including when building without Git.
const publicFiles = [
  ".gitattributes", ".gitignore", "ATTRIBUTION.md",
  "FX-THIRD-PARTY-NOTICES.md", "LICENCE", "README.md", "images",
  "index.html", "package.json", "package-lock.json", "vite.config.js",
  "scripts", "src", "src-tauri/build.rs", "src-tauri/capabilities",
  "src-tauri/Cargo.lock", "src-tauri/Cargo.toml", "src-tauri/icons",
  "src-tauri/src", "src-tauri/tauri.conf.json", "scenes",
];

export function packageRelease(root, targetDir) {
  const { name, version } = JSON.parse(readFileSync(join(root, "package.json"), "utf8"));
  const releaseDir = join(root, "release");
  const versionDir = join(releaseDir, `v${version}`);
  mkdirSync(versionDir, { recursive: true });

  const executable = `${name}.exe`;
  const source = `${name}-source-v${version}.zip`;
  copyFileSync(join(targetDir, "release", executable), join(versionDir, executable));
  execFileSync("tar.exe", [
    "-acf", join(versionDir, source),
    "--exclude", "AGENTS.md", "--exclude", ".agents", "--exclude", ".codex",
    ...publicFiles,
  ], { cwd: root });

  const hashes = [executable, source].map(file => {
    const digest = createHash("sha256").update(readFileSync(join(versionDir, file))).digest("hex");
    return `${digest}  ${file}\n`;
  });
  writeFileSync(join(versionDir, "SHA256SUMS.txt"), hashes.join(""));
  copyFileSync(join(versionDir, executable), join(releaseDir, executable));
  console.log(`Packaged v${version} in ${versionDir}`);
}
