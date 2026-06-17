import { execFileSync } from "node:child_process";
import { existsSync, mkdtempSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { fileURLToPath } from "node:url";

const tempDirectory = mkdtempSync(join(tmpdir(), "kirby-ocpp-pack-check-"));
const installDirectory = join(tempDirectory, "install");
const npmCacheDirectory = join(tempDirectory, "npm-cache");
const projectDirectory = fileURLToPath(new URL("..", import.meta.url));
const npmEnvironment = { ...process.env, npm_config_cache: npmCacheDirectory };

try {
  const tarballName = execFileSync("npm", ["pack", "--pack-destination", tempDirectory, "--silent"], {
    cwd: projectDirectory,
    env: npmEnvironment,
    encoding: "utf8",
    stdio: ["ignore", "pipe", "inherit"]
  }).trim();
  const tarballPath = join(tempDirectory, tarballName);

  execFileSync("npm", ["install", "--prefix", installDirectory, "--no-audit", "--no-fund", tarballPath], {
    env: npmEnvironment,
    stdio: "inherit"
  });

  const binPath = join(installDirectory, "node_modules", ".bin", "kirby-ocpp");
  const version = execFileSync(binPath, ["--version"], { encoding: "utf8" }).trim();
  if (version !== "0.1.0") {
    throw new Error(`Expected kirby-ocpp --version to be 0.1.0, got ${version}`);
  }

  const help = execFileSync(binPath, ["--help"], { encoding: "utf8" });
  if (!help.includes("Terminal OCPP charge point emulator") || help.includes("--id-tag")) {
    throw new Error("Packed CLI help is missing expected text or still exposes --id-tag");
  }

  const schemaPath = join(
    installDirectory,
    "node_modules",
    "kirby-ocpp",
    "dist",
    "ocpp",
    "schemas",
    "json",
    "BootNotification.json"
  );
  if (!existsSync(schemaPath)) {
    throw new Error(`Packed install is missing schema: ${schemaPath}`);
  }

  console.log(`Packed install check passed for ${tarballName}`);
} finally {
  rmSync(tempDirectory, { recursive: true, force: true });
}
