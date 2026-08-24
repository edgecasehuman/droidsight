# Changelog

All notable changes to droidsight are recorded here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and versions follow
[Semantic Versioning](https://semver.org/spec/v2.0.0.html).

The release workflow reads the section matching the tag it is building and
publishes it as the release notes, above the generated checksum table. A tag
whose version has no section here fails the release rather than publishing an
empty page, so this file is edited in the same commit that bumps the version.

## [Unreleased]

### Added

- The release archives carry a build provenance attestation, verifiable with
  `gh attestation verify <archive> --repo edgecasehuman/droidsight`. The npm
  packages have always been published with provenance; the standalone archives
  were unsigned and unattested, with a published SHA256SUMS as their only check.
- MCP Registry metadata is published from the release workflow through GitHub
  OIDC, with a pinned publisher and no long-lived token in the repository.

### Fixed

- The launcher could lose its own error messages. Every diagnostic was a
  `console.error` followed immediately by `process.exit`, and writes to stderr
  are asynchronous when stderr is a pipe — which is exactly what an MCP client
  provides. The messages explaining a missing platform package or an
  unexecutable binary are now written synchronously, so they survive the exit
  they are reporting.
- The npm publish step's provenance path.

### Changed

- The README and CONTRIBUTING name NASM as a build prerequisite. `openh264-sys2`
  assembles Cisco's decoder with it and no platform preinstalls it, so both
  `cargo install droidsight` and a source build failed inside a transitive
  dependency, where the cause is hard to read. It was documented only in
  comments in the CI workflows.
- The README points at the crates.io and MCP Registry listings alongside `npx`.

## [1.0.0] - 2026-08-12

### Added

- Initial public release. An MCP server that drives a real Android device over
  ADB and attaches the resulting screen to every action, so an agent does not
  need a separate screenshot call. One native binary; the runtime is the binary
  plus `adb`, with no Python, Appium, scrcpy, ffmpeg, or Node.
- A background H.264 `screenrecord` stream decoded in process, so the screen
  following an action comes from cache rather than a fresh capture.
- Every image carries the coordinate space it was produced in, so a model can
  tap what it just looked at without inferring a scale factor.
- OCR and template matching for screens the accessibility tree cannot read —
  Flutter, React Native, canvas, games.
- Thirty-one tools published by default, and two more only when
  `DROIDSIGHT_ALLOW_SHELL=1` is set.
- Distribution as five platform binaries through npm with provenance
  attestations, a launcher package, and an entry in the MCP Registry.

[Unreleased]: https://github.com/edgecasehuman/droidsight/compare/v1.0.0...HEAD
[1.0.0]: https://github.com/edgecasehuman/droidsight/releases/tag/v1.0.0
