import { spawn } from "node:child_process";
import { existsSync, readFileSync } from "node:fs";
import { delimiter, dirname, resolve } from "node:path";
import { homedir } from "node:os";
import { fileURLToPath } from "node:url";
import { packageRelease } from "./package-release.mjs";

const root = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const [command, ...args] = process.argv.slice(2);
if (!["dev", "build"].includes(command)) {
  throw new Error("Use npm run dev or npm run build.");
}

const env = { ...process.env };
const pathKey = Object.keys(env).find(key => key.toLowerCase() === "path") || "PATH";
env[pathKey] = [
  resolve(env.CARGO_HOME || resolve(homedir(), ".cargo"), "bin"),
  dirname(process.execPath),
  env[pathKey] || ""
].join(delimiter);

const pathFile = resolve(root, ".local/game-path.txt");
if (command === "dev" && !env.WAW_ROOT && existsSync(pathFile)) {
  const saved = readFileSync(pathFile, "utf8").trim();
  if (saved) {
    env.WAW_ROOT = saved;
  }
}

env.CARGO_TARGET_DIR = resolve(root, env.CARGO_TARGET_DIR || "src-tauri/target");
const child = spawn(process.execPath, [
  resolve(root, "node_modules/@tauri-apps/cli/tauri.js"), command, ...args
], { cwd: root, env, stdio: "inherit" });

child.on("error", error => {
  console.error(error.message);
  process.exitCode = 1;
});

child.on("exit", code => {
  process.exitCode = code ?? 1;
  if (code !== 0 || command !== "build") {
    return;
  }
  try {
    packageRelease(root, env.CARGO_TARGET_DIR);
  } catch (error) {
    console.error(`Could not archive the built EXE: ${error.message}`);
    process.exitCode = 1;
  }
});

