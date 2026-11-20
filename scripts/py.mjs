// Runs a Python tool from the repo root with the project's virtual environment, in isolated mode (-I).
// Usage: node scripts/py.mjs tools/validate_ontology.py [args...]
// Needed because the venv's python lives at .venv/Scripts/python.exe on Windows and .venv/bin/python elsewhere
// (CI installs packages into the system Python, so it falls back to `python`).
import { spawnSync } from "node:child_process";
import { existsSync } from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";

const root = path.join(path.dirname(fileURLToPath(import.meta.url)), "..");
const candidates = [path.join(root, ".venv", "Scripts", "python.exe"), path.join(root, ".venv", "bin", "python")];
const python = candidates.find(existsSync) ?? (process.platform === "win32" ? "python" : "python3");

const result = spawnSync(python, ["-I", ...process.argv.slice(2)], { cwd: root, stdio: "inherit" });
if (result.error) {
  console.error(`could not start ${python}: ${result.error.message}`);
  process.exit(1);
}
process.exit(result.status ?? 1);
