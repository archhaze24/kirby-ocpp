import { cpSync, existsSync, mkdirSync, rmSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const root = join(dirname(fileURLToPath(import.meta.url)), "..");
const source = join(root, "src/ocpp/schemas");
const target = join(root, "dist/ocpp/schemas");

if (!existsSync(source)) {
  throw new Error(`Schema source directory does not exist: ${source}`);
}

rmSync(target, { recursive: true, force: true });
mkdirSync(dirname(target), { recursive: true });
cpSync(source, target, {
  recursive: true,
  filter: (sourcePath) => !sourcePath.endsWith(".DS_Store")
});
