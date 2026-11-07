import { deflateSync } from "node:zlib";
import { createHash } from "node:crypto";
import {
  copyFile,
  cp,
  mkdir,
  mkdtemp,
  readFile,
  readdir,
  rm,
  writeFile,
} from "node:fs/promises";
import { tmpdir } from "node:os";
import { existsSync } from "node:fs";
import { dirname, delimiter, join, resolve, sep } from "node:path";
import { fileURLToPath } from "node:url";
import { execFileSync } from "node:child_process";

const root = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const runtimeDir = join(root, "runtime", "netease-api");
const resourceDir = join(root, "src", "resources", "netease-api");
const cacheDir = join(root, ".local-validation", "netease-node");
const bridgeSource = join(root, "scripts", "netease-bridge.cjs");
const nodeVersion = "24.21.0";

function distributionFor(target) {
  const normalized = target.toLowerCase();
  const arch = normalized.includes("aarch64") || normalized.includes("arm64")
    ? "arm64"
    : normalized.includes("x86_64") || normalized.includes("x64")
      ? "x64"
      : null;
  if (!arch) throw new Error(`Unsupported Netease runtime architecture: ${target}`);
  if (normalized.includes("windows") || normalized.startsWith("win-")) {
    return { archive: `node-v${nodeVersion}-win-${arch}.zip`, executable: "node.exe" };
  }
  if (normalized.includes("apple") || normalized.includes("darwin") || normalized.startsWith("macos-")) {
    return { archive: `node-v${nodeVersion}-darwin-${arch}.tar.gz`, executable: "bin/node" };
  }
  if (normalized.includes("linux")) {
    return { archive: `node-v${nodeVersion}-linux-${arch}.tar.gz`, executable: "bin/node" };
  }
  throw new Error(`Unsupported Netease runtime target: ${target}`);
}

async function download(url) {
  let failure;
  for (let attempt = 0; attempt < 3; attempt++) {
    try {
      const response = await fetch(url, { signal: AbortSignal.timeout(60000) });
      if (!response.ok) throw new Error(`Download failed (${response.status}): ${url}`);
      return Buffer.from(await response.arrayBuffer());
    } catch (error) { failure = error; }
  }
  throw failure;
}

function run(executable, args) {
  if (process.platform === "win32" && executable === "npm") {
    const npmCli = process.env.PATH.split(delimiter)
      .map((entry) => join(entry, "node_modules", "npm", "bin", "npm-cli.js"))
      .find(existsSync);
    if (!npmCli) throw new Error("Could not locate npm-cli.js on PATH");
    execFileSync(process.execPath, [npmCli, ...args], { cwd: root, stdio: "inherit" });
    return;
  }
  execFileSync(executable, args, { cwd: root, stdio: "inherit" });
}

function assertRuntimeOutput(path) {
  const resolved = resolve(path);
  const prefix = `${resolve(resourceDir)}${sep}`;
  if (resolved !== resolve(resourceDir) && !resolved.startsWith(prefix)) {
    throw new Error(`Refusing to modify path outside generated runtime: ${resolved}`);
  }
}

function assertTempOutput(path) {
  const resolved = resolve(path);
  const prefix = `${resolve(tmpdir())}${sep}netease-node-`;
  if (!resolved.startsWith(prefix)) throw new Error(`Refusing to remove unexpected temporary path: ${resolved}`);
}

const runtimeManifest = JSON.parse(await readFile(join(runtimeDir, "package.json"), "utf8"));
const expectedApiVersion = runtimeManifest.dependencies["@neteasecloudmusicapienhanced/api"];
if (!/^\d+\.\d+\.\d+$/.test(expectedApiVersion)) {
  throw new Error(`Expected an exact API semver pin, got: ${expectedApiVersion}`);
}
const triple = process.env.NETEASE_RUNTIME_TARGET ?? process.env.TAURI_ENV_TARGET_TRIPLE;
const hostPlatform = process.platform === "win32" ? "windows" : process.platform;
const distribution = distributionFor(triple ?? `${hostPlatform}-${process.arch}`);
const baseUrl = `https://nodejs.org/dist/v${nodeVersion}`;
const checksumFile = (await download(`${baseUrl}/SHASUMS256.txt`)).toString("utf8");
const checksum = checksumFile
  .split(/\r?\n/)
  .map((line) => line.trim().split(/\s+/))
  .find(([, filename]) => filename === distribution.archive)?.[0];
if (!checksum) throw new Error(`No official Node checksum found for ${distribution.archive}`);

await mkdir(cacheDir, { recursive: true });
const archivePath = join(cacheDir, `${distribution.archive}.${checksum}`);
let archive;
let archiveCached = true;
try {
  archive = await readFile(archivePath);
  if (createHash("sha256").update(archive).digest("hex") !== checksum) throw new Error("bad cached checksum");
} catch {
  archiveCached = false;
  archive = await download(`${baseUrl}/${distribution.archive}`);
  if (createHash("sha256").update(archive).digest("hex") !== checksum) {
    throw new Error(`Node archive checksum mismatch for ${distribution.archive}`);
  }
  await writeFile(archivePath, archive);
}

const lock = await readFile(join(runtimeDir, "package-lock.json"));
const lockHash = createHash("sha256").update("no-bin-links-v1\n").update(lock).digest("hex");
const marker = join(runtimeDir, "node_modules", ".netease-lock-sha256");
let installedLockHash = "";
try { installedLockHash = await readFile(marker, "utf8"); } catch {}
const dependenciesCached = installedLockHash === lockHash;
if (!dependenciesCached) {
  run("npm", ["ci", "--prefix", runtimeDir, "--omit=dev", "--ignore-scripts", "--bin-links=false"]);
  await writeFile(marker, lockHash);
}

const tempDir = await mkdtemp(join(tmpdir(), "netease-node-"));
try {
  const extractDir = join(tempDir, "extract");
  await mkdir(extractDir);
  await writeFile(join(tempDir, distribution.archive), archive);
  const archiveFolder = distribution.archive.replace(/\.(zip|tar\.gz)$/, "");
  const tar = process.platform === "win32" ? join(process.env.SystemRoot || "C:\\Windows", "System32", "tar.exe") : "tar";
  run(tar, ["-C", extractDir, "-xf", join(tempDir, distribution.archive), `${archiveFolder}/${distribution.executable}`, `${archiveFolder}/LICENSE`]);
  const executableSource = join(extractDir, archiveFolder, distribution.executable);

  assertRuntimeOutput(resourceDir);
  await rm(resourceDir, { recursive: true, force: true });
  await mkdir(dirname(join(resourceDir, distribution.executable)), { recursive: true });
  await copyFile(executableSource, join(resourceDir, distribution.executable));
  await copyFile(join(runtimeDir, "package.json"), join(resourceDir, "package.json"));
  await copyFile(join(runtimeDir, "package-lock.json"), join(resourceDir, "package-lock.json"));
  await cp(join(runtimeDir, "node_modules"), join(resourceDir, "node_modules"), {
    recursive: true,
    filter: (source) => !source.split(sep).includes(".bin"),
  });
  await copyFile(bridgeSource, join(resourceDir, "bridge.cjs"));
  await copyFile(join(extractDir, archiveFolder, "LICENSE"), join(resourceDir, "NODE-LICENSE"));

  const apiDir = join(resourceDir, "node_modules", "@neteasecloudmusicapienhanced", "api");
  const apiVersion = JSON.parse(await readFile(join(apiDir, "package.json"), "utf8")).version;
  if (apiVersion !== expectedApiVersion) throw new Error(`Unexpected API version: ${apiVersion}; expected ${expectedApiVersion}`);
  const packageFiles = await readdir(apiDir);
  const entries = [];
  async function collect(directory, prefix = "") {
    for (const entry of (await readdir(directory, { withFileTypes: true })).sort((a, b) => a.name.localeCompare(b.name))) {
      const relative = prefix ? `${prefix}/${entry.name}` : entry.name;
      if (entry.isDirectory()) await collect(join(directory, entry.name), relative);
      else if (entry.isFile()) entries.push([relative, await readFile(join(directory, entry.name))]);
      else throw new Error(`Unsupported runtime entry: ${relative}`);
    }
  }
  await collect(resourceDir);
  const header = Buffer.alloc(12);
  header.write("MSRAPI01", 0, "ascii");
  header.writeUInt32LE(entries.length, 8);
  const pieces = [header];
  for (const [name, bytes] of entries) {
    const encoded = Buffer.from(name, "utf8");
    const lengths = Buffer.alloc(12);
    lengths.writeUInt32LE(encoded.length, 0);
    lengths.writeBigUInt64LE(BigInt(bytes.length), 4);
    pieces.push(lengths, encoded, bytes);
  }
  const payload = deflateSync(Buffer.concat(pieces), { level: 9 });
  const payloadHash = createHash("sha256").update(payload).digest("hex");
  await writeFile(join(root, "src", "resources", "netease-runtime.bin"), payload);
  await writeFile(join(root, "src", "resources", "netease-runtime.sha256"), payloadHash);
  console.log(`Embedded runtime: ${entries.length} files compressed into ${payload.length} bytes.`);
  console.log(`Prepared NetEase API ${apiVersion} with Node ${nodeVersion} (${distribution.archive}); Node archive cache: ${archiveCached ? "hit" : "miss"}; production dependency cache: ${dependenciesCached ? "hit" : "miss"}; API entry files: ${packageFiles.length}.`);
} finally {
  assertTempOutput(tempDir);
  await rm(tempDir, { recursive: true, force: true });
}
