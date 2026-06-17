import { cpSync, existsSync, mkdirSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const root = join(dirname(fileURLToPath(import.meta.url)), "..");
const source = join(root, "src/ocpp/schemas");
const target = join(root, "dist/ocpp/schemas");

if (!existsSync(source)) {
  throw new Error(`Schema source directory does not exist: ${source}`);
}

mkdirSync(dirname(target), { recursive: true });
cpSync(source, target, { recursive: true });
