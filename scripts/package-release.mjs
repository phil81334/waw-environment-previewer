import { execFileSync } from "node:child_process";
import { createHash } from "node:crypto";
import { copyFileSync, existsSync, mkdirSync, readFileSync, writeFileSync } from "node:fs";
import { join } from "node:path";

// List build inputs explicitly so local game paths, bake outputs and previous
// releases stay out of the source download, including when building without Git.
const publicFiles = [
  ".gitattributes", ".gitignore", "ATTRIBUTION.md", "LICENCE", "README.md", "images",
  "index.html", "package.json", "package-lock.json", "vite.config.js",
  "scripts", "src", "src-tauri/build.rs", "src-tauri/capabilities",
  "src-tauri/Cargo.lock", "src-tauri/Cargo.toml", "src-tauri/icons",
  "src-tauri/src", "src-tauri/tauri.conf.json", "scenes",
];

export function packageRelease(root, targetDir) {
  const { name, version } = JSON.parse(readFileSync(join(root, "package.json"), "utf8"));
  const versionDir = join(root, "release", `v${version}`);
  const executable = `${name}.exe`;
  const builtExecutable = join(targetDir, "release", executable);
  if (existsSync(join(versionDir, "release-info.md")) && existsSync(join(versionDir, executable))) {
    console.log(`Release v${version} is already archived. Your new build is at ${builtExecutable}`);
    return;
  }

  const attachmentsDir = join(root, ".local", "release-artifacts", `v${version}`);
  mkdirSync(versionDir, { recursive: true });
  mkdirSync(attachmentsDir, { recursive: true });

  const source = `${name}-source-v${version}.zip`;
  copyFileSync(builtExecutable, join(versionDir, executable));
  execFileSync("tar.exe", [
    "-acf", join(attachmentsDir, source),
    "--exclude", "AGENTS.md", "--exclude", ".agents", "--exclude", ".codex",
    ...publicFiles,
  ], { cwd: root });

  const hashes = [[versionDir, executable], [attachmentsDir, source]].map(([directory, file]) => {
    const digest = createHash("sha256").update(readFileSync(join(directory, file))).digest("hex");
    return `${digest}  ${file}\n`;
  });
  writeFileSync(join(attachmentsDir, "SHA256SUMS.txt"), hashes.join(""));
  console.log(`Packaged v${version} in ${versionDir}\nSource ZIP and checksums: ${attachmentsDir}`);
}
