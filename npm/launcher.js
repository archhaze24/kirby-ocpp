#!/usr/bin/env node
// Launcher: runs the native binary shipped by the matching platform package. One script
// serves both launcher packages: build.mjs copies it to bin/<name>.js and the binary name
// (`kirby-ocpp` or `kirby-csms`) is taken from the script's own file name. Both binaries
// live in the same @archhaze24/kirby-ocpp-<platform> packages.
"use strict";

const { spawnSync } = require("node:child_process");
const path = require("node:path");

const NAME = path.basename(__filename, ".js");

// Candidate packages per platform, in order of preference. The musl build is fully static,
// so on Linux x64 it is also a valid fallback when the glibc package was not installed.
const PLATFORMS = {
  "linux-x64": ["@archhaze24/kirby-ocpp-linux-x64", "@archhaze24/kirby-ocpp-linux-x64-musl"],
  "linux-arm64": ["@archhaze24/kirby-ocpp-linux-arm64"],
  "darwin-x64": ["@archhaze24/kirby-ocpp-darwin-x64"],
  "darwin-arm64": ["@archhaze24/kirby-ocpp-darwin-arm64"],
  "win32-x64": ["@archhaze24/kirby-ocpp-win32-x64"],
};

// True on a musl-based Linux (Alpine etc.): glibc reports its version, musl does not.
function isMusl() {
  if (process.platform !== "linux") return false;
  try {
    const report = process.report.getReport();
    const header = typeof report === "string" ? JSON.parse(report).header : report.header;
    if (header && "glibcVersionRuntime" in header) return !header.glibcVersionRuntime;
  } catch {
    // fall through to the file check
  }
  try {
    return require("node:fs").readdirSync("/lib").some((name) => name.startsWith("ld-musl-"));
  } catch {
    return false;
  }
}

function binaryPath() {
  const key = `${process.platform}-${process.arch}`;
  const candidates = PLATFORMS[key];
  if (!candidates) {
    throw new Error(
      `${NAME} has no prebuilt binary for ${key}. Supported: ${Object.keys(PLATFORMS).join(", ")}. ` +
        `Build from source instead: cargo install ${NAME}`,
    );
  }
  const ordered = key === "linux-x64" && isMusl() ? [...candidates].reverse() : candidates;
  const exe = process.platform === "win32" ? `${NAME}.exe` : NAME;
  for (const pkg of ordered) {
    try {
      const manifest = require.resolve(`${pkg}/package.json`);
      return path.join(path.dirname(manifest), "bin", exe);
    } catch {
      // not installed, try the next candidate
    }
  }
  throw new Error(
    `The platform package ${ordered[0]} is not installed (npm skipped the optional dependency). ` +
      `Reinstall without --omit=optional, or run: cargo install ${NAME}`,
  );
}

let binary;
try {
  binary = binaryPath();
} catch (error) {
  console.error(`${NAME}: ${error.message}`);
  process.exit(1);
}

const result = spawnSync(binary, process.argv.slice(2), { stdio: "inherit" });
if (result.error) {
  console.error(`${NAME}: could not run ${binary}: ${result.error.message}`);
  process.exit(1);
}
if (result.signal) {
  process.kill(process.pid, result.signal);
} else {
  process.exit(result.status ?? 1);
}
