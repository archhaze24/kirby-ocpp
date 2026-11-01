#!/usr/bin/env node
// Launcher: runs the native kirby-ocpp binary shipped by the matching platform package.
"use strict";

const { spawnSync } = require("node:child_process");
const path = require("node:path");

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
      `kirby-ocpp has no prebuilt binary for ${key}. Supported: ${Object.keys(PLATFORMS).join(", ")}. ` +
        "Build from source instead: cargo install kirby-ocpp",
    );
  }
  const ordered = key === "linux-x64" && isMusl() ? [...candidates].reverse() : candidates;
  const exe = process.platform === "win32" ? "kirby-ocpp.exe" : "kirby-ocpp";
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
      "Reinstall without --omit=optional, or run: cargo install kirby-ocpp",
  );
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
