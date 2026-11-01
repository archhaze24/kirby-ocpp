#!/usr/bin/env node
// Launcher: runs the native kirby-ocpp binary shipped by the matching platform package.
"use strict";

const { spawnSync } = require("node:child_process");
const path = require("node:path");

const PLATFORMS = {
  "linux-x64": "@archhaze24/kirby-ocpp-linux-x64",
  "linux-arm64": "@archhaze24/kirby-ocpp-linux-arm64",
  "darwin-x64": "@archhaze24/kirby-ocpp-darwin-x64",
  "darwin-arm64": "@archhaze24/kirby-ocpp-darwin-arm64",
  "win32-x64": "@archhaze24/kirby-ocpp-win32-x64",
};

function binaryPath() {
  const key = `${process.platform}-${process.arch}`;
  const pkg = PLATFORMS[key];
  if (!pkg) {
    throw new Error(
      `kirby-ocpp has no prebuilt binary for ${key}. Supported: ${Object.keys(PLATFORMS).join(", ")}. ` +
        "Build from source instead: cargo install kirby-ocpp",
    );
  }
  let manifest;
  try {
    manifest = require.resolve(`${pkg}/package.json`);
  } catch {
    throw new Error(
      `The platform package ${pkg} is not installed (npm skipped the optional dependency). ` +
        "Reinstall without --omit=optional, or run: cargo install kirby-ocpp",
    );
  }
  const exe = process.platform === "win32" ? "kirby-ocpp.exe" : "kirby-ocpp";
  return path.join(path.dirname(manifest), "bin", exe);
}

let binary;
try {
  binary = binaryPath();
} catch (error) {
  console.error(`kirby-ocpp: ${error.message}`);
  process.exit(1);
}

const result = spawnSync(binary, process.argv.slice(2), { stdio: "inherit" });
if (result.error) {
  console.error(`kirby-ocpp: could not run ${binary}: ${result.error.message}`);
  process.exit(1);
}
if (result.signal) {
  process.kill(process.pid, result.signal);
} else {
  process.exit(result.status ?? 1);
}
