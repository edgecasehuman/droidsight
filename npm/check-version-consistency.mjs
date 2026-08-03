#!/usr/bin/env node
// Verify that everything carrying a version or a package identity agrees.
//
//   node npm/check-version-consistency.mjs
//
// The version is written out four times -- Cargo.toml builds the binary,
// server.json is what the MCP registry lists, server.json's package entry says
// which npm release backs that listing, and npm/droidsight/package.json is the
// launcher itself. Two identities are written twice more: server.json's name has
// to equal the launcher's mcpName, and server.json's package identifier has to
// equal the launcher's package name.
//
// release.yml already checks the four versions against the tag, and the mcpName
// against server.json. It does that on a tag, which is the expensive place to
// find out: a tag that fails here is spent, and the first droidsight tag is the
// one that cannot be re-cut cheaply. This runs on every push and pull request so
// the disagreement is caught while it is still a one-line edit.
//
// The identity checks matter for a reason that outlives the version bump. The
// MCP registry proves ownership by reading the *published* npm package and
// requiring its mcpName to equal server.json's name, so a mismatch cannot be
// corrected in place -- the fix has to ride a version that has not shipped yet.
// The same is true of the identifier: a listing pointing at the wrong package
// name is a listing for someone else's package.
//
// optionalDependencies are deliberately NOT checked. The launcher publish step
// rewrites every one of them to the release version before publishing, so the
// committed values never reach the registry and holding them to the manifest
// version would be enforcing something that does not matter.

import { readFileSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const here = dirname(fileURLToPath(import.meta.url));
const root = resolve(here, "..");

const problems = [];

const read = (...parts) => readFileSync(join(root, ...parts), "utf8");
const readJson = (...parts) => JSON.parse(read(...parts));

// The same slice release.yml takes with sed: the [package] table only, so a
// version key belonging to some other table cannot be picked up by mistake.
function cargoPackageVersion(text) {
  let inPackage = false;
  for (const line of text.split(/\r?\n/)) {
    if (/^\[/.test(line)) {
      if (inPackage) break;
      inPackage = line.trim() === "[package]";
      continue;
    }
    if (!inPackage) continue;
    const match = /^version\s*=\s*"([^"]+)"/.exec(line.trim());
    if (match) return match[1];
  }
  return undefined;
}

const server = readJson("server.json");
const launcher = readJson("npm", "droidsight", "package.json");

const versions = [
  ["Cargo.toml", cargoPackageVersion(read("Cargo.toml"))],
  ["server.json version", server.version],
  ["server.json packages[0].version", server.packages?.[0]?.version],
  ["npm/droidsight/package.json version", launcher.version],
];

for (const [source, value] of versions) {
  if (!value) {
    problems.push(`${source} has no version`);
  }
}

const distinct = [...new Set(versions.map(([, value]) => value))];
if (distinct.length > 1) {
  problems.push(
    "the version is not the same everywhere\n" +
      versions.map(([source, value]) => `  ${source}: ${value}`).join("\n")
  );
}

function identity(description, actual, expected, expectedSource) {
  if (actual !== expected) {
    problems.push(
      `${description} is ${actual}, but ${expectedSource} is ${expected}`
    );
  }
}

identity(
  "npm/droidsight/package.json mcpName",
  launcher.mcpName,
  server.name,
  "server.json name"
);

identity(
  "server.json packages[0].identifier",
  server.packages?.[0]?.identifier,
  launcher.name,
  "npm/droidsight/package.json name"
);

if (problems.length) {
  for (const problem of problems) {
    console.error(`error: ${problem}`);
  }
  process.exit(1);
}

console.error(
  `version agrees across all four declarations: ${distinct[0]}\n` +
    `identity agrees: ${server.name} <-> ${launcher.name}`
);
