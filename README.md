# droidsight

[![CI](https://github.com/edgecasehuman/droidsight/actions/workflows/ci.yml/badge.svg)](https://github.com/edgecasehuman/droidsight/actions/workflows/ci.yml)
[![License: MIT](https://img.shields.io/badge/License-MIT-blue.svg)](LICENSE)

An MCP server that drives a real Android device over ADB — and hands the agent
back the screen its action produced.

One native binary. The server has no Python, Appium, scrcpy, or ffmpeg
dependency and runs no Node at all — `npx` below is only a convenient
installer — so the runtime is the binary plus `adb`. A background H.264
`screenrecord` stream is decoded in process, so the screen that follows an
action is attached to the tool result automatically instead of costing a second
round trip to ask what happened.

Every image carries the coordinate space it was produced in, so a model can tap
what it just looked at without guessing a scale factor. When the accessibility
tree comes back empty — Flutter, React Native, canvas, games — OCR and template
matching still find the target.

This is a powerful local controller, not a sandbox. Run it only against a device
and an MCP client you trust.

## Install

```bash
npx -y @edgecasehuman/droidsight
```

Or build from source:

```bash
cargo build --locked --release --bin droidsight
```

The server is `target/release/droidsight`. It speaks newline-delimited
JSON-RPC 2.0 over stdin and stdout.

### Client configuration

```json
{
  "mcpServers": {
    "android": {
      "command": "npx",
      "args": ["-y", "@edgecasehuman/droidsight"],
      "env": {
        "DROIDSIGHT_DEVICE_SERIAL": "<serial from `adb devices`>"
      }
    }
  }
}
```

ADB must be reachable through `PATH`, `ANDROID_SDK_ROOT`, `ANDROID_HOME`, or an
explicit `DROIDSIGHT_ADB_PATH`. If more than one authorized device is visible
the server refuses to guess; set `DROIDSIGHT_DEVICE_SERIAL`.

## Tools

Thirty-one tools are published by default. Two more appear only when
`DROIDSIGHT_ALLOW_SHELL=1` is set, and are listed last.

### Seeing the screen

| Tool | Purpose |
|---|---|
| `mcp_android_vision_query` | Screenshot, hierarchy, OCR, and element or template search. Its `elements` and `tap_element` actions avoid coordinate arithmetic entirely. |
| `mcp_android_vision_stream` | Start, stop, or read the background H.264 stream. |
| `mcp_android_get_view_hierarchy` | The accessibility tree as structured JSON. |
| `mcp_android_smart_wait` | Block until an element appears, or time out. |

### Driving the device

| Tool | Purpose |
|---|---|
| `mcp_android_input_act` | Tap, type, key events, swipe, smart tap, and IME control. |
| `mcp_android_tap_text` | Scan for text and tap it the instant it appears, retrying until timeout. Avoids the miss between a screenshot and a tap. |
| `mcp_android_run_flow` | Run a bounded, fully prevalidated sequence of safe actions. |
| `mcp_android_record_gesture` | Record raw touch input for a fixed duration. |
| `mcp_android_play_gesture` | Replay a recorded gesture timeline. |

### Applications

| Tool | Purpose |
|---|---|
| `mcp_android_app_manage` | Launch, stop, list, install, and read crash logs. Uninstall, clear data, permission, enable, and disable additionally require `confirm_destructive`. |
| `mcp_android_app_instrumentation` | Deep state inspection: activity, window, process list, stack traces. |
| `mcp_android_open_url` | Open a URL. |
| `mcp_android_start_intent` | Start an arbitrary intent. |

### Device and system state

| Tool | Purpose |
|---|---|
| `mcp_android_device_control` | Clipboard, battery, device info, lock state, unlock, rotation. |
| `mcp_android_system_control` | Accessibility services and the draw-over-other-apps permission. **Grants screen-read and input-injection authority across every app.** |
| `mcp_android_network_control` | Wi-Fi, mobile data, HTTP proxy, phone calls, SMS, wireless ADB pairing. |
| `mcp_android_sensor_control` | Mock battery level and charging status; GPS mocking is emulator-only. |
| `mcp_android_check_health` | Connectivity and responsiveness of the selected device. |
| `mcp_android_check_debug_exposure` | Report which developer settings are enabled and therefore visible to apps. |

### Diagnostics and data

| Tool | Purpose |
|---|---|
| `mcp_android_diagnostic_stream` | Read logcat, clear it, or read buffered raw and semantic events. |
| `mcp_android_read_recent_events` | Buffered device events from the optional monitor. |
| `mcp_android_log_filter` | Filter logs by regex, tag, or priority. |
| `mcp_android_get_notifications` | Dump posted notifications, including unredacted message content. |
| `mcp_android_file_system` | List, read, push, and pull files, confined to `DROIDSIGHT_LOCAL_ROOT`. |
| `mcp_android_forensics_control` | Query an on-device SQLite database, hash a file, or irreversibly delete an application's data. |

### Capture and presence

| Tool | Purpose |
|---|---|
| `mcp_android_start_recording` | Begin a screen recording, up to 180 seconds, written to the device. |
| `mcp_android_stop_recording` | Stop it. The file stays on the device. |
| `mcp_android_companion` | Post a notification, show a transient message, or open a URL for whoever is holding the phone. |
| `mcp_android_sentinel_control` | Register a watched package. See [Background enforcement](#background-enforcement) before using it. |
| `mcp_android_start_session`, `mcp_android_stop_session` | Session markers. They acknowledge the call and hold no server state. |

### Only with `DROIDSIGHT_ALLOW_SHELL=1`

| Tool | Purpose |
|---|---|
| `mcp_android_run_shell` | Run an arbitrary device shell command. No filtering of any kind. |
| `mcp_android_run_macro` | Run up to 100 shell commands in sequence, stopping at the first failure. |

## Coordinate space

Screenshots are downscaled to 720 pixels wide by default, so a coordinate read
off the returned pixels is usually **not** a device coordinate. Every tool that
returns an image also returns a `metadata.image` object describing how to
convert one:

```json
{
  "image": {
    "width": 720, "height": 1600,
    "device_width": 1080, "device_height": 2400,
    "origin_x": 0, "origin_y": 0,
    "scale": 1.5,
    "coordinate_space": "image",
    "note": "To convert a coordinate (x, y) read from this image into a device coordinate: device_x = 0 + x * 1.5, device_y = 0 + y * 1.5."
  }
}
```

When `coordinate_space` is `device`, the image is already 1:1 and coordinates
can be used directly. Request `max_width: 1440` to reduce or remove downscaling.
Crops additionally report a non-zero `origin_x`/`origin_y` that must be added
after scaling.

Taps, `uiautomator` hierarchy bounds, OCR boxes, and template matches are all in
device coordinates. The background stream runs `screenrecord` without `--size`,
so decoded frames are native resolution and share that one space.

You can avoid the arithmetic entirely. The `mcp_android_vision_query` tool's
`elements` action returns an indexed snapshot with a precomputed center per
element, and its `tap_element` action takes that `snapshot_id` and an `index`.

## Environment variables

| Variable | Effect |
|---|---|
| `DROIDSIGHT_DEVICE_SERIAL` | The only Android target the process may use. Mandatory when ADB reports multiple authorized devices. |
| `DROIDSIGHT_ADB_PATH` | Explicit path to the ADB executable. |
| `DROIDSIGHT_LOCAL_ROOT` | Confines host paths used by APK installation and file push/pull. Defaults to the process working directory. |
| `DROIDSIGHT_DEVICE_PIN` | Numeric PIN for UI actions that need automatic unlock. Supply at runtime only; never in source or a checked-in MCP configuration. |
| `DROIDSIGHT_ALLOW_SHELL=1` | Publishes the arbitrary shell and macro tools. This grants authority equivalent to broad ADB shell access. Off by default. |
| `DROIDSIGHT_DEBUG_LOG` | Path to a persistent debug log. Logging to a file is strictly opt-in; without this, warnings go to stderr and no file is written. |
| `DROIDSIGHT_LOG` | Tracing filter. Defaults to `warn`, or `debug` when a debug log path is set. |
| `DROIDSIGHT_EVENTS` | Starts the long-running device event monitor. Requires `DROIDSIGHT_DEVICE_SERIAL`. |
| `DROIDSIGHT_SENTINEL=1` | Starts the background enforcement loop described under [Background enforcement](#background-enforcement). Off by default: it re-applies device state on a timer, including unlocking the screen. |

## Continuous vision cache

One background H.264 `screenrecord` stream starts at process startup and the
latest decoded RGB frame is kept in memory. Screenshot tools and input
observations read that cache, so a snapshot is available immediately once the
first frame decodes. A static screen's last frame stays valid while the stream
runs; it does not expire merely because the encoder stopped emitting unchanged
pixels.

The stream allows 20 seconds for the first decodable frame before reconnecting.
After that, silence is treated as a static screen, while EOF and read errors
still reconnect. Starting or stopping a stream synchronously clears its frame
cache so a previous session's image cannot be reused. The
`mcp_android_vision_stream` `stop` action suspends capture until an explicit
`start`; shutdown drops the owned ADB stream and leaves no `screenrecord`
process behind.

Continuous capture has privacy, battery, CPU, and wireless-bandwidth costs. The
device screen may contain sensitive information even though cached frames stay
in process memory until a tool returns one. Use the explicit stop action when
capture should be suspended.

## MCP transport

Stdout carries one JSON-RPC value per line; diagnostics go to stderr. The server
supports `initialize`, `tools/list`, and `tools/call`, plus the initialized
notification and legacy `mcp.*` aliases. Batch arrays are handled member by
member and produce one response-array line; notification-only batches produce no
output and empty batches are rejected. Clients must complete the ordered
`initialize` request and `notifications/initialized` notification handshake
before listing or calling tools; premature calls return JSON-RPC error `-32002`.
Input frames are limited to 16 MiB; oversized or invalid UTF-8 frames are
rejected without preventing the next valid request from being processed.

Published text from logs, events, notifications, device files, hierarchy and OCR
results, crash and forensic reports, instrumentation, shell/macro output, and
aggregate flows is limited to 256 KiB. Truncated responses carry
`metadata.truncation` with the strategy, original size, returned size, and
configured limit; chronological feeds retain their newest tail while
file-like output retains its head.

Finite ADB subprocesses drain stdout and stderr concurrently and retain at most
4 MiB from each stream while counting all observed bytes, which prevents pipe
deadlocks and unbounded capture. The optional event monitor owns its ADB child
and reader thread and is cancelled, killed, joined, and reaped during transport
shutdown rather than being left detached.

OCR is an optional integration with an external Tesseract executable. Discovery
probes are limited to five seconds and recognition to 30 seconds; timed-out
children are terminated and reaped before the tool returns an error.

Smoke test:

```bash
printf '%s\n' \
  '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}' \
  '{"jsonrpc":"2.0","method":"notifications/initialized"}' \
  '{"jsonrpc":"2.0","id":2,"method":"tools/list","params":{}}' \
  | cargo run --quiet --bin droidsight
```

## Destructive operations

Some tools change device state irreversibly. `pm clear` deletes an
application's databases, preferences, accounts, and credentials, and it is
reachable two ways. Both refuse to run without `"confirm_destructive": true`:

- `mcp_android_forensics_control` with `"action": "clear_app_data"`, named for
  what it does rather than for a cache eviction.
- `mcp_android_app_manage` with `"action": "clear_data"`. The same tool gates
  `uninstall`, `permission`, `enable`, and `disable` behind the same argument,
  because each of them removes an application, its data, or a security control.

The guard is checked before any ADB command is issued, so a refused call does
not touch the device. It is a guardrail against an unintended call, not a
security boundary: a client that can invoke tools can also set the flag.

The gate covers unintended calls to those specific tools. It is not a
containment boundary, and several operations of comparable impact are not behind
it at all:

- `mcp_android_system_control` enables an accessibility service, which grants
  full screen-read and input-injection access across every app on the device,
  and grants the draw-over-other-apps permission.
- `mcp_android_network_control` sets a global HTTP proxy, forgets saved Wi-Fi
  networks, and places real outbound phone calls.
- `mcp_android_sensor_control` overrides sensor readings such as battery level
  and status; its GPS location mocking uses the emulator console and applies
  only to emulators, not physical devices.

## Arbitrary shell access

`DROIDSIGHT_ALLOW_SHELL=1` publishes two tools that pass command strings to the
device shell verbatim: `mcp_android_run_shell`, and `mcp_android_run_macro` for
batches of up to 100 commands.

**No command filtering of any kind is performed.** The environment variable is
the entire control. Once set, these tools are a strict superset of every gated
operation above — `pm clear`, `pm uninstall`, and file deletion are all reachable
without `confirm_destructive`, because the shell path never consults it. Leave
the variable unset unless the connected MCP client is as trusted as a local
shell on the device.

## Background enforcement

`DROIDSIGHT_SENTINEL=1` starts a loop that wakes every five seconds and, for
each watched package, re-applies the state it was told to hold: waking and
unlocking the screen with a PIN supplied when the watch was registered, enabling
an accessibility service, granting the overlay permission, and granting a list
of runtime permissions. No watches exist until a client registers one, so an
enabled sentinel with an empty watch list only ticks.

Two consequences are worth stating plainly:

- **It reverses manual changes.** Revoking a permission or disabling the
  accessibility service for a watched package is undone within five seconds.
  Remove the watch first.
- **It does not ask for `confirm_destructive`.** That gate covers an operator
  calling `mcp_android_app_manage`; the loop invokes the same underlying
  operation directly, because re-granting is the entire purpose of a watch.
  Registering a watch is the consent step for everything that watch will
  subsequently do on its own.

The loop performs no network I/O beyond the local ADB connection and contacts no
remote service.

## Build and test

The repository pins its Rust toolchain and keeps device-mutating scenarios out
of the default test suite.

```bash
cargo fmt --all -- --check
cargo clippy --locked --all-targets -- -D warnings
cargo test --locked -- --test-threads=1
cargo build --locked --release --bin droidsight
```

These gates run on Linux, macOS, and Windows and pass without warning
suppressions.

See [CONTRIBUTING.md](CONTRIBUTING.md) before changing device-affecting code.
[SECURITY.md](SECURITY.md) documents private reporting, sensitive-evidence
redaction, and the ADB, host, MCP, filesystem, logging, and shell trust
boundaries. Participation is governed by
[CODE_OF_CONDUCT.md](CODE_OF_CONDUCT.md).

## Maturity

The host-only suite covers protocol parsing, schemas, serialization, ADB command
construction, concurrent discovery, reconnect invalidation, device selection,
subprocess deadlines, JSON-RPC batches, path confinement, shell quoting, output
budgets, event-monitor teardown, hierarchy parsing, flow validation, stream
framing and lifecycle, static-frame cache behavior, image encoding and
coordinate metadata, Samsung keyguard/window parsing, and intent failure
handling.

The release binary has been exercised over wireless debugging against a single
Samsung Galaxy A-series device running Android 13, covering device state,
battery, apps, hierarchy and
element snapshots, OCR, screenshots and sequences, continuous H.264 capture,
input and navigation, intents, logs, notifications, Wi-Fi scanning, file round
trips, hashing, sessions, gesture capture, recording, rotation restoration,
crash listing, and cleanup. Destructive application, permission, accessibility,
overlay, telephony, network-mutation, GPS, and battery-mocking operations are
deliberately excluded from that matrix. Other vendors and Android releases still
need their own qualification.

## H.264 decoding

The vision cache decodes H.264 in process using [openh264], which builds Cisco's
BSD-2-Clause C++ implementation from source. Cisco's royalty coverage applies to
the binaries Cisco itself publishes and does not transfer to a self-compiled
build. If you redistribute binaries of this project, satisfying any applicable
H.264 patent licensing is your responsibility.

[openh264]: https://github.com/cisco/openh264

## License

MIT. See [LICENSE](LICENSE).
