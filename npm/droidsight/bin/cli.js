#!/usr/bin/env node
"use strict";

// Thin launcher for the platform-specific binary installed as an optional
// dependency. npm installs only the package whose `os`/`cpu` match the host, so
// exactly one of these resolves.
//
// This process sits between the MCP client and the server on stdio, so it must
// not write anything to stdout. Diagnostics go to stderr.

const { spawn } = require("node:child_process");
const { constants } = require("node:os");
const { writeSync } = require("node:fs");

const FORWARDED_SIGNALS = ["SIGINT", "SIGTERM", "SIGHUP"];

// Every diagnostic below is followed immediately by process.exit, and that pair
// is exactly where console.error loses its output: writes to stderr are only
// synchronous for a TTY or a regular file. To a pipe on POSIX they are queued,
// and process.exit does not drain the queue. An MCP client always hands this
// process a pipe for stderr, so the plain form would drop these messages in the
// one situation they were written for, while looking correct in every terminal
// test. writeSync bypasses the queue. A non-blocking pipe can refuse the write
// with EAGAIN, which means retry rather than failed; EPIPE means the reader is
// already gone, and there is no one left to tell.
function fail(message) {
  const buffer = Buffer.from(`${message}\n`, "utf8");
  let written = 0;
  while (written < buffer.length) {
    try {
      written += writeSync(2, buffer, written);
    } catch (error) {
      if (error.code === "EAGAIN") {
        continue;
      }
      if (error.code !== "EPIPE") {
        // Last resort: report through the lossy path rather than not at all.
        console.error(message);
      }
      break;
    }
  }
  process.exit(1);
}

const PACKAGES = {
  "linux-x64": "@edgecasehuman/droidsight-linux-x64",
  "linux-arm64": "@edgecasehuman/droidsight-linux-arm64",
  "darwin-x64": "@edgecasehuman/droidsight-darwin-x64",
  "darwin-arm64": "@edgecasehuman/droidsight-darwin-arm64",
  "win32-x64": "@edgecasehuman/droidsight-win32-x64",
};

const key = `${process.platform}-${process.arch}`;
const pkg = PACKAGES[key];

if (!pkg) {
  fail(
    `droidsight: no prebuilt binary for ${key}.\n` +
      `Supported: ${Object.keys(PACKAGES).join(", ")}.\n` +
      `Build from source instead: cargo install droidsight (needs NASM on the PATH).`
  );
}

let binary;
try {
  binary = require.resolve(`${pkg}/bin/${process.platform === "win32" ? "droidsight.exe" : "droidsight"}`);
} catch {
  fail(
    `droidsight: the platform package ${pkg} is not installed.\n` +
      `This usually means the install ran with --no-optional or --omit=optional.\n` +
      `Reinstall without those flags, or install it directly: npm i ${pkg}`
  );
}

// stdio: "inherit" hands the real descriptors to the child, so the JSON-RPC
// stream is never copied through this process or re-encoded.
//
// A missing file is reported asynchronously, on the error event below. A file
// that exists but cannot be executed is not: spawn throws synchronously for
// it, so the handler never sees that case at all. That is the likelier failure
// here, because require.resolve above has already proven the file exists --
// what is left is a binary quarantined by antivirus, truncated by an
// interrupted download, or built for another architecture. Without the catch,
// the user gets a Node stack trace instead of a sentence naming the file.
let child;
try {
  child = spawn(binary, process.argv.slice(2), { stdio: "inherit" });
} catch (error) {
  fail(`droidsight: failed to start ${binary}: ${error.message}`);
}

child.on("error", (error) => {
  fail(`droidsight: failed to start ${binary}: ${error.message}`);
});

// Forward termination so a client killing the launcher stops the server too.
for (const signal of FORWARDED_SIGNALS) {
  process.on(signal, () => {
    if (!child.killed) {
      child.kill(signal);
    }
  });
}

child.on("exit", (code, signal) => {
  if (signal) {
    // Reproduce the child's signal death rather than flattening it to an exit
    // code, so supervisors see why it stopped. The handlers installed above
    // would otherwise swallow the signal we raise on ourselves and let the
    // process fall off the end of the event loop reporting success; removing
    // the last listener for a signal restores its default disposition.
    for (const forwarded of FORWARDED_SIGNALS) {
      process.removeAllListeners(forwarded);
    }
    // Windows has no real signal delivery, so set the shell convention first
    // and let an actual signal death override it on POSIX.
    process.exitCode = 128 + (constants.signals[signal] ?? 0);
    process.kill(process.pid, signal);
    return;
  }
  process.exit(code ?? 0);
});
