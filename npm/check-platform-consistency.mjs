#!/usr/bin/env node
// Verify that everything which enumerates the supported platforms agrees.
//
//   node npm/check-platform-consistency.mjs
//
// The list is written out four times: platforms.json drives the build, the
// release matrix decides what actually gets compiled, optionalDependencies
// decides what npm installs, and the launcher decides what it will look for.
// Nothing links them, and each disagreement fails differently and late:
//
//   missing from the matrix          -> optionalDependencies points at a
//                                       version that was never published, and
//                                       every install on every platform fails
//   missing from optionalDependencies-> npm installs no binary, and the
//                                       launcher reports it as --omit=optional
//   missing from the launcher        -> "no prebuilt binary for <platform>" on
//                                       a platform whose binary is right there
//
// None of that is caught by a test, a type, or a build. It is caught here, and
// the release workflow runs this before it builds anything.

import { readFileSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const here = dirname(fileURLToPath(import.meta.url));
const root = resolve(here, "..");
const scope = "@edgecasehuman/droidsight";

const platforms = JSON.parse(readFileSync(join(here, "platforms.json"), "utf8"));
const expected = platforms.map((entry) => entry.pkg).sort();

const problems = [];

function compare(source, actual) {
  const sorted = [...actual].sort();
  if (sorted.join(",") !== expected.join(",")) {
    const missing = expected.filter((pkg) => !sorted.includes(pkg));
    const extra = sorted.filter((pkg) => !expected.includes(pkg));
    problems.push(
      `${source} does not match platforms.json` +
        (missing.length ? `\n  missing: ${missing.join(", ")}` : "") +
        (extra.length ? `\n  unexpected: ${extra.join(", ")}` : "")
    );
  }
}

// platforms.json itself: the pkg key is what the launcher computes from
// process.platform and process.arch, so it has to be exactly `<os>-<cpu>`.
for (const entry of platforms) {
  if (entry.pkg !== `${entry.os}-${entry.cpu}`) {
    problems.push(
      `platforms.json entry '${entry.pkg}' is not '${entry.os}-${entry.cpu}', ` +
        `so the launcher's process.platform-process.arch lookup can never find it`
    );
  }
}

const manifest = JSON.parse(
  readFileSync(join(here, "droidsight", "package.json"), "utf8")
);
compare(
  "npm/droidsight/package.json optionalDependencies",
  Object.keys(manifest.optionalDependencies ?? {}).map((name) =>
    name.replace(`${scope}-`, "")
  )
);

// Read the launcher's own table rather than importing it: requiring the file
// would run its platform check and exit.
const launcher = readFileSync(join(here, "droidsight", "bin", "cli.js"), "utf8");
compare(
  "npm/droidsight/bin/cli.js PACKAGES",
  [...launcher.matchAll(/"([a-z0-9]+-[a-z0-9]+)":\s*"@edgecasehuman\/droidsight-([a-z0-9-]+)"/g)]
    .map(([, key, pkg]) => {
      if (key !== pkg) {
        problems.push(`cli.js maps '${key}' to the '${pkg}' package`);
      }
      return key;
    })
);

// The launcher hardcodes the executable's filename, while
// build-platform-package.mjs takes it from platforms.json. Both agree today and
// nothing links them. If they ever disagree, require.resolve fails and the
// launcher reports "the platform package is not installed" -- which is a lie:
// the package is installed, only the name inside it differs. That message sends
// the user to reinstall, which cannot fix it.
const binaryNames = /process\.platform === "win32"\s*\?\s*"([^"]+)"\s*:\s*"([^"]+)"/.exec(
  launcher
);
if (!binaryNames) {
  problems.push(
    "cli.js no longer selects the binary name with a process.platform ternary, " +
      "so this check can no longer read it -- update the check with the code"
  );
} else {
  const [, windowsBinary, otherBinary] = binaryNames;
  for (const entry of platforms) {
    const expected = entry.os === "win32" ? windowsBinary : otherBinary;
    if (entry.bin !== expected) {
      problems.push(
        `platforms.json builds ${entry.pkg} as '${entry.bin}', but cli.js looks ` +
          `for '${expected}' on ${entry.os}`
      );
    }
  }
}

const workflow = readFileSync(
  join(root, ".github", "workflows", "release.yml"),
  "utf8"
);
compare(
  ".github/workflows/release.yml build matrix",
  [...workflow.matchAll(/^\s*-\s*\{\s*target:\s*([\w-]+),.*?pkg:\s*([\w-]+),\s*bin:\s*(\S+?)\s*\}/gm)].map(
    ([, target, pkg, bin]) => {
      const platform = platforms.find((entry) => entry.pkg === pkg);
      if (platform && platform.target !== target) {
        problems.push(
          `release.yml builds ${pkg} for ${target}, platforms.json says ${platform.target}`
        );
      }
      if (platform && platform.bin !== bin) {
        problems.push(
          `release.yml names the ${pkg} binary ${bin}, platforms.json says ${platform.bin}`
        );
      }
      return pkg;
    }
  )
);

if (problems.length) {
  for (const problem of problems) {
    console.error(`error: ${problem}`);
  }
  process.exit(1);
}

console.error(`platform list agrees across all four declarations: ${expected.join(", ")}`);
