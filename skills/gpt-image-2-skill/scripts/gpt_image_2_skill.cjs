#!/usr/bin/env node

const fs = require("node:fs");
const os = require("node:os");
const path = require("node:path");
const childProcess = require("node:child_process");

const CLI_NAME = "gpt-image-2-skill";
const BIN_ENV = "GPT_IMAGE_2_SKILL_BIN";
const APP_BIN_ENV = "GPT_IMAGE_2_SKILL_APP_BIN";
const SKILL_ROOT = path.resolve(__dirname, "..");

function wantsJson(argv) {
  return argv.includes("--json");
}

function emitFailure(argv, message, code = "runtime_unavailable", detail = null) {
  if (wantsJson(argv)) {
    const payload = {
      ok: false,
      error: {
        code,
        message,
      },
    };
    if (detail !== null) {
      payload.error.detail = detail;
    }
    process.stdout.write(`${JSON.stringify(payload)}\n`);
  } else {
    process.stderr.write(`${message}\n`);
  }
  return 1;
}

function isExecutableFile(filePath) {
  try {
    const stats = fs.statSync(filePath);
    if (!stats.isFile()) {
      return false;
    }
    if (process.platform === "win32") {
      return true;
    }
    fs.accessSync(filePath, fs.constants.X_OK);
    return true;
  } catch {
    return false;
  }
}

function pathEntries() {
  return (process.env.PATH || "")
    .split(path.delimiter)
    .map((entry) => entry.trim())
    .filter(Boolean);
}

function executableExtensions() {
  if (process.platform !== "win32") {
    return [""];
  }
  return (process.env.PATHEXT || ".EXE;.CMD;.BAT;.COM")
    .split(";")
    .map((entry) => entry.trim())
    .filter(Boolean);
}

function resolveExecutable(name) {
  if (path.isAbsolute(name) || name.includes(path.sep)) {
    return isExecutableFile(name) ? name : null;
  }
  for (const directory of pathEntries()) {
    for (const extension of executableExtensions()) {
      const candidate = path.join(directory, process.platform === "win32" ? `${name}${extension}` : name);
      if (isExecutableFile(candidate)) {
        return candidate;
      }
    }
  }
  return null;
}

function resolveFromEnvBinary() {
  const configured = (process.env[BIN_ENV] || "").trim();
  if (!configured) {
    return null;
  }
  const candidate = path.resolve(configured);
  if (!isExecutableFile(candidate)) {
    return null;
  }
  return { argvPrefix: [candidate], cwd: null, source: "env" };
}

function resolveFromSkillScripts() {
  const binaryName = process.platform === "win32" ? `${CLI_NAME}.exe` : CLI_NAME;
  const candidates = [
    path.join(SKILL_ROOT, "scripts", binaryName),
    path.join(SKILL_ROOT, binaryName),
  ];
  for (const candidate of candidates) {
    if (isExecutableFile(candidate)) {
      return { argvPrefix: [candidate], cwd: null, source: "skill-scripts" };
    }
  }
  return null;
}

function resolveFromBundledBinary() {
  const binaryName = process.platform === "win32" ? `${CLI_NAME}.exe` : CLI_NAME;
  for (const { triple } of detectTargets()) {
    const candidate = path.join(SKILL_ROOT, "bin", triple, binaryName);
    if (isExecutableFile(candidate)) {
      const runtime = { argvPrefix: [candidate], cwd: null, source: "bundled" };
      if (runtimeSupportsSharedConfig(runtime)) {
        return runtime;
      }
    }
  }
  return null;
}

function resolveFromPath() {
  const binary = resolveExecutable(CLI_NAME);
  if (!binary) {
    return null;
  }
  return { argvPrefix: [binary], cwd: null, source: "path" };
}

function appBundleCandidates() {
  const binaryName = process.platform === "win32" ? `${CLI_NAME}.exe` : CLI_NAME;
  const candidates = [];
  const configured = (process.env[APP_BIN_ENV] || "").trim();
  if (configured) {
    candidates.push(path.resolve(configured));
  }
  if (process.platform === "darwin") {
    candidates.push(
      `/Applications/GPT Image 2.app/Contents/Resources/bin/${binaryName}`,
      path.join(os.homedir(), "Applications", `GPT Image 2.app/Contents/Resources/bin/${binaryName}`)
    );
  } else if (process.platform === "win32") {
    for (const root of [process.env.LOCALAPPDATA, process.env.PROGRAMFILES]) {
      if (root) {
        candidates.push(path.join(root, "GPT Image 2", "resources", "bin", binaryName));
      }
    }
  } else {
    candidates.push(
      `/opt/gpt-image-2/resources/bin/${binaryName}`,
      path.join(os.homedir(), ".local", "share", "gpt-image-2", "bin", binaryName)
    );
  }
  return candidates;
}

function resolveFromAppBundle() {
  for (const candidate of appBundleCandidates()) {
    if (isExecutableFile(candidate)) {
      return { argvPrefix: [candidate], cwd: null, source: "app" };
    }
  }
  return null;
}

function detectLibc() {
  if (process.platform !== "linux") {
    return null;
  }
  if (process.report && typeof process.report.getReport === "function") {
    const report = process.report.getReport();
    if (report && report.header && report.header.glibcVersionRuntime) {
      return "gnu";
    }
  }
  return fs.existsSync("/etc/alpine-release") ? "musl" : "gnu";
}

function detectTargets() {
  if (process.platform === "darwin") {
    if (process.arch === "arm64") {
      return [{ triple: "aarch64-apple-darwin", extension: "" }];
    }
    if (process.arch === "x64") {
      return [{ triple: "x86_64-apple-darwin", extension: "" }];
    }
    throw new Error(`Unsupported macOS architecture: ${process.arch}`);
  }
  if (process.platform === "linux") {
    const arch = process.arch === "arm64" ? "aarch64" : process.arch === "x64" ? "x86_64" : null;
    if (!arch) {
      throw new Error(`Unsupported Linux architecture: ${process.arch}`);
    }
    const libc = detectLibc();
    const preferred = { triple: `${arch}-unknown-linux-${libc}`, extension: "" };
    if (libc === "gnu") {
      return [preferred, { triple: `${arch}-unknown-linux-musl`, extension: "" }];
    }
    return [preferred];
  }
  if (process.platform === "win32") {
    const arch = process.arch === "arm64" ? "aarch64" : process.arch === "x64" ? "x86_64" : null;
    if (!arch) {
      throw new Error(`Unsupported Windows architecture: ${process.arch}`);
    }
    return [{ triple: `${arch}-pc-windows-msvc`, extension: ".exe" }];
  }
  throw new Error(`Unsupported platform: ${process.platform}`);
}

function runtimeSupportsSharedConfig(runtime) {
  const [command, ...prefixArgs] = runtime.argvPrefix;
  const result = childProcess.spawnSync(command, [...prefixArgs, "--json", "config", "path"], {
    cwd: runtime.cwd || undefined,
    encoding: "utf8",
    stdio: ["ignore", "pipe", "pipe"],
  });
  if (result.error || result.status !== 0) {
    return false;
  }
  try {
    const payload = JSON.parse(result.stdout);
    return payload && payload.ok === true && payload.command === "config path";
  } catch {
    return false;
  }
}

function resolveRuntime() {
  for (const resolver of [
    resolveFromEnvBinary,
    resolveFromSkillScripts,
    resolveFromBundledBinary,
    resolveFromPath,
    resolveFromAppBundle,
  ]) {
    const runtime = resolver();
    if (runtime && runtimeSupportsSharedConfig(runtime)) {
      return runtime;
    }
  }
  const binaryName = process.platform === "win32" ? `${CLI_NAME}.exe` : CLI_NAME;
  throw new Error(
    `gpt-image-2-skill runtime is unavailable. Place ${binaryName} next to this skill (scripts/${binaryName}) or set ${BIN_ENV}.`
  );
}

function main(argv = process.argv.slice(2)) {
  try {
    const runtime = resolveRuntime();
    const [command, ...prefixArgs] = runtime.argvPrefix;
    const result = childProcess.spawnSync(command, [...prefixArgs, ...argv], {
      cwd: runtime.cwd || undefined,
      stdio: "inherit",
    });
    if (result.error) {
      throw result.error;
    }
    return result.status ?? 1;
  } catch (error) {
    return emitFailure(argv, error instanceof Error ? error.message : String(error));
  }
}

process.exit(main());
