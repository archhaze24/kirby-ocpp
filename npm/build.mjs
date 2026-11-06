#!/usr/bin/env node
// Assembles the npm packages for a release from the GitHub release assets.
//
//   node npm/build.mjs v0.2.0 [--assets <dir>] [--out npm/dist]
//
// Without --assets the archives are downloaded from the GitHub release of that tag.
// Output (one directory per package, ready for `npm publish`):
//   <out>/kirby-ocpp-<os>-<arch>[-musl]/   platform packages @archhaze24/kirby-ocpp-<os>-<arch> (both binaries in bin/)
//   <out>/kirby-ocpp/               launcher package `kirby-ocpp`
//   <out>/kirby-csms/               launcher package `kirby-csms`
// Publish the platform packages first, the launcher packages last (see RELEASING.md).
import { execFileSync } from "node:child_process";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";

const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");
const NOTICES = path.join(root, "crates", "kirby-ocpp", "THIRD_PARTY_NOTICES.md");
const NOTICES_CSMS = path.join(root, "crates", "kirby-csms", "THIRD_PARTY_NOTICES.md");
const BINARIES = ["kirby-ocpp", "kirby-csms"];
const LAUNCHER = path.join(root, "npm", "launcher.js");
const REPO = "archhaze24/kirby-ocpp";

const PLATFORMS = [
  { pkg: "@archhaze24/kirby-ocpp-linux-x64", target: "x86_64-unknown-linux-gnu", os: "linux", cpu: "x64", libc: "glibc", ext: "tar.gz" },
  { pkg: "@archhaze24/kirby-ocpp-linux-x64-musl", target: "x86_64-unknown-linux-musl", os: "linux", cpu: "x64", libc: "musl", ext: "tar.gz" },
  { pkg: "@archhaze24/kirby-ocpp-linux-arm64", target: "aarch64-unknown-linux-gnu", os: "linux", cpu: "arm64", libc: "glibc", ext: "tar.gz" },
  { pkg: "@archhaze24/kirby-ocpp-linux-arm64-musl", target: "aarch64-unknown-linux-musl", os: "linux", cpu: "arm64", libc: "musl", ext: "tar.gz" },
  { pkg: "@archhaze24/kirby-ocpp-darwin-x64", target: "x86_64-apple-darwin", os: "darwin", cpu: "x64", ext: "tar.gz" },
  { pkg: "@archhaze24/kirby-ocpp-darwin-arm64", target: "aarch64-apple-darwin", os: "darwin", cpu: "arm64", ext: "tar.gz" },
  { pkg: "@archhaze24/kirby-ocpp-win32-x64", target: "x86_64-pc-windows-msvc", os: "win32", cpu: "x64", ext: "zip", win: true },
];

const MARKER = ".kirby-npm-dist";

// The output directory is wiped on every build, so refuse anything that is not clearly a
// scratch dist: the repo root or its ancestors, the home dir, the filesystem root, and any
// non-empty directory that was not created by a previous run (no marker file).
function checkOut(out) {
  const real = fs.existsSync(out) ? fs.realpathSync(out) : out;
  const contains = (parent, child) => {
    const rel = path.relative(parent, child);
    return rel === "" || (!rel.startsWith("..") && !path.isAbsolute(rel));
  };
  const protectedDirs = [os.homedir(), path.parse(real).root];
  if (contains(real, root) || contains(real, fs.realpathSync(root)) || protectedDirs.some((dir) => path.resolve(dir) === real)) {
    throw new Error(`refusing to use ${out} as --out: it is the repo root, one of its ancestors, the home directory or /`);
  }
  if (fs.existsSync(out)) {
    if (!fs.statSync(out).isDirectory()) throw new Error(`--out ${out} exists and is not a directory`);
    if (fs.readdirSync(out).length > 0 && !fs.existsSync(path.join(out, MARKER))) {
      throw new Error(`refusing to overwrite non-empty ${out}: it has no ${MARKER} marker from a previous build`);
    }
  }
}

function parseArgs(argv) {
  const args = { tag: undefined, assets: undefined, out: path.join(root, "npm", "dist") };
  for (let i = 0; i < argv.length; i++) {
    const arg = argv[i];
    if (arg === "--assets") args.assets = path.resolve(argv[++i]);
    else if (arg === "--out") args.out = path.resolve(argv[++i]);
    else if (!args.tag && /^v\d+\.\d+\.\d+/.test(arg)) args.tag = arg;
    else throw new Error(`unexpected argument: ${arg}`);
  }
  if (!args.tag) throw new Error("usage: node npm/build.mjs vX.Y.Z [--assets <dir>] [--out <dir>]");
  return args;
}

// Output directory name of a package: the scope is dropped (`@archhaze24/kirby-ocpp-linux-x64` -> `kirby-ocpp-linux-x64`).
function dirName(pkg) {
  return pkg.split("/").pop();
}

function assetName(tag, platform) {
  return `kirby-ocpp-${tag}-${platform.target}.${platform.ext}`;
}

async function obtain(tag, platform, assetsDir, workDir) {
  const name = assetName(tag, platform);
  if (assetsDir) {
    const file = path.join(assetsDir, name);
    if (!fs.existsSync(file)) throw new Error(`missing asset ${file}`);
    return file;
  }
  const url = `https://github.com/${REPO}/releases/download/${tag}/${name}`;
  const response = await fetch(url);
  if (!response.ok) throw new Error(`download failed (${response.status}): ${url}`);
  const file = path.join(workDir, name);
  fs.writeFileSync(file, Buffer.from(await response.arrayBuffer()));
  return file;
}

function extract(archive, into) {
  fs.mkdirSync(into, { recursive: true });
  if (archive.endsWith(".zip")) {
    // bsdtar (macOS, Windows 10+) reads zip; GNU tar does not, so fall back to unzip.
    try {
      execFileSync("tar", ["-xf", archive, "-C", into], { stdio: "pipe" });
    } catch {
      execFileSync("unzip", ["-q", "-o", archive, "-d", into], { stdio: "inherit" });
    }
  } else {
    execFileSync("tar", ["-xzf", archive, "-C", into], { stdio: "inherit" });
  }
}

function findFile(dir, fileName) {
  for (const entry of fs.readdirSync(dir, { withFileTypes: true })) {
    const full = path.join(dir, entry.name);
    if (entry.isDirectory()) {
      const found = findFile(full, fileName);
      if (found) return found;
    } else if (entry.name === fileName) {
      return full;
    }
  }
  return undefined;
}

function writeJson(file, value) {
  fs.writeFileSync(file, `${JSON.stringify(value, null, 2)}\n`);
}

async function main() {
  const { tag, assets, out } = parseArgs(process.argv.slice(2));
  const version = tag.slice(1);
  const template = JSON.parse(fs.readFileSync(path.join(root, "npm", "kirby-ocpp", "package.json"), "utf8")); // shared license/author/repository
  const work = fs.mkdtempSync(path.join(os.tmpdir(), "kirby-ocpp-npm-"));
  checkOut(out);
  fs.rmSync(out, { recursive: true, force: true });
  fs.mkdirSync(out, { recursive: true });
  fs.writeFileSync(path.join(out, MARKER), "created by npm/build.mjs; safe to delete and overwrite\n");

  const optionalDependencies = {};
  for (const platform of PLATFORMS) {
    const archive = await obtain(tag, platform, assets, work);
    const unpacked = path.join(work, dirName(platform.pkg));
    extract(archive, unpacked);
    const dir = path.join(out, dirName(platform.pkg));
    fs.mkdirSync(path.join(dir, "bin"), { recursive: true });
    for (const name of BINARIES) {
      const exe = platform.win ? `${name}.exe` : name;
      const binary = findFile(unpacked, exe);
      if (!binary) throw new Error(`${exe} not found in ${archive}`);
      const target = path.join(dir, "bin", exe);
      fs.copyFileSync(binary, target);
      fs.chmodSync(target, 0o755);
    }
    fs.copyFileSync(path.join(root, "LICENSE"), path.join(dir, "LICENSE"));
    fs.copyFileSync(NOTICES, path.join(dir, "THIRD_PARTY_NOTICES.md"));
    fs.copyFileSync(NOTICES_CSMS, path.join(dir, "THIRD_PARTY_NOTICES-kirby-csms.md"));
    fs.writeFileSync(
      path.join(dir, "README.md"),
      `# ${platform.pkg}\n\nPrebuilt \`kirby-ocpp\` and \`kirby-csms\` binaries for ${platform.os}/${platform.cpu}${platform.libc ? ` (${platform.libc})` : ""}. Install \`kirby-ocpp\` or \`kirby-csms\` instead of this package: https://github.com/${REPO}\n`,
    );
    writeJson(path.join(dir, "package.json"), {
      name: platform.pkg,
      version,
      description: `kirby-ocpp and kirby-csms binaries for ${platform.os}-${platform.cpu}${platform.libc ? ` (${platform.libc})` : ""}`,
      license: template.license,
      author: template.author,
      repository: template.repository,
      os: [platform.os],
      cpu: [platform.cpu],
      ...(platform.libc ? { libc: [platform.libc] } : {}),
      files: ["bin", "README.md", "LICENSE", "THIRD_PARTY_NOTICES.md", "THIRD_PARTY_NOTICES-kirby-csms.md"],
    });
    optionalDependencies[platform.pkg] = version;
    console.log(`built ${platform.pkg}@${version}`);
  }

  const launchers = [
    { bin: "kirby-ocpp", notices: NOTICES },
    { bin: "kirby-csms", notices: NOTICES_CSMS },
  ];
  for (const { bin, notices } of launchers) {
    const dir = path.join(out, bin);
    const manifest = JSON.parse(fs.readFileSync(path.join(root, "npm", bin, "package.json"), "utf8"));
    fs.mkdirSync(path.join(dir, "bin"), { recursive: true });
    const script = path.join(dir, "bin", `${bin}.js`);
    fs.copyFileSync(LAUNCHER, script);
    fs.chmodSync(script, 0o755);
    for (const doc of ["README.md", "LICENSE"]) {
      fs.copyFileSync(path.join(root, doc), path.join(dir, doc));
    }
    fs.copyFileSync(notices, path.join(dir, "THIRD_PARTY_NOTICES.md"));
    writeJson(path.join(dir, "package.json"), { ...manifest, version, optionalDependencies });
    console.log(`built ${manifest.name}@${version}`);
  }
  fs.rmSync(work, { recursive: true, force: true });
}

main().catch((error) => {
  console.error(`build.mjs: ${error.message}`);
  process.exit(1);
});
