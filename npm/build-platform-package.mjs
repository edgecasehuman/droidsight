#!/usr/bin/env node
// Assemble one platform-specific npm package around a freshly built binary.
//
//   node npm/build-platform-package.mjs --pkg linux-x64 \
//        --binary target/x86_64-unknown-linux-gnu/release/droidsight \
//        --version 1.0.0
//
// Output lands in npm/dist/droidsight-<pkg>/ ready for `npm publish`.

import { chmodSync, copyFileSync, mkdirSync, readFileSync, writeFileSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const here = dirname(fileURLToPath(import.meta.url));
const root = resolve(here, "..");

function arg(name) {
  const index = process.argv.indexOf(`--${name}`);
  if (index === -1 || index + 1 >= process.argv.length) {
    throw new Error(`missing required argument --${name}`);
  }
  return process.argv[index + 1];
}

const pkgKey = arg("pkg");
const binaryPath = resolve(arg("binary"));
const version = arg("version");

const platforms = JSON.parse(readFileSync(join(here, "platforms.json"), "utf8"));
const platform = platforms.find((entry) => entry.pkg === pkgKey);
if (!platform) {
  throw new Error(`unknown platform '${pkgKey}'; expected one of ${platforms.map((p) => p.pkg).join(", ")}`);
}

const name = `@edgecasehuman/droidsight-${platform.pkg}`;
const outDir = join(here, "dist", `droidsight-${platform.pkg}`);
mkdirSync(join(outDir, "bin"), { recursive: true });

const manifest = {
  name,
  version,
  description: `Prebuilt droidsight binary for ${platform.os} ${platform.cpu}.`,
  license: "MIT",
  repository: {
    type: "git",
    url: "git+https://github.com/edgecasehuman/droidsight.git",
  },
  os: [platform.os],
  cpu: [platform.cpu],
  files: ["bin", "LICENSE"],
  // Yarn PnP keeps dependencies zipped by default; a native executable has to
  // exist on disk to be spawned.
  preferUnplugged: true,
};
if (platform.libc) {
  manifest.libc = [platform.libc];
}

writeFileSync(join(outDir, "package.json"), `${JSON.stringify(manifest, null, 2)}\n`);
copyFileSync(join(root, "LICENSE"), join(outDir, "LICENSE"));

const destination = join(outDir, "bin", platform.bin);
copyFileSync(binaryPath, destination);
if (platform.os !== "win32") {
  chmodSync(destination, 0o755);
}

console.error(`built ${name}@${version} -> ${outDir}`);
