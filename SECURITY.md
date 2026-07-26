# Security Policy

## Supported versions

Security fixes are made on the default branch. Until the project publishes a
formal support matrix, older commits, binaries, forks, and unofficial builds
should not be assumed to receive security updates.

## Reporting a vulnerability

Please do not disclose a suspected vulnerability in a public issue, discussion,
pull request, log, or transcript.

Use GitHub's **Security** tab and its private vulnerability-reporting form when
that feature is available. Include:

- the affected commit or version;
- a concise description of the impact and required preconditions;
- reproduction steps or a minimal proof of concept;
- whether a real Android device or host was affected; and
- any suggested mitigation, if known.

Remove device serials, account data, authentication material, personal data,
screenshots, UI dumps, logs, and other secrets before attaching evidence. The
maintainers should acknowledge a private report, assess its scope, coordinate a
fix, and agree on disclosure timing before publication. If private reporting is
not enabled, ask the repository owner to provide a private reporting channel
without including vulnerability details in the request.

## Trust and authority boundary

This server is an automation bridge, not a security boundary. A client that can
invoke its tools may exercise the authority of the selected Android Debug Bridge
(ADB) device and of the host process running the server.

In particular:

- An ADB-authorized device grants broad inspection and mutation capabilities,
  including input injection, application control, file access permitted to the
  shell user, screenshots, logs, and device-state changes.
- An unlocked, rooted, debug, test, or specially provisioned device can expose
  more authority than a normal production device.
- Arbitrary-shell functionality, when explicitly enabled, substantially expands
  the command surface. It should remain disabled unless the operator accepts
  that risk and trusts every connected MCP client.
- Device output is untrusted input. UI hierarchies, filenames, logs, package
  metadata, OCR text, and subprocess output may be malformed, unexpectedly
  large, or crafted to influence downstream consumers.
- Host filesystem access must remain confined to explicitly configured roots.
  Do not run the server with access to secrets or directories that clients do
  not need.
- Device serial selection is a safety control. Operators should configure an
  explicit serial when more than one device could be present and must verify the
  target before running mutating tools.
- Stdio transport does not authenticate the process on the other end. The
  launcher is responsible for client trust, process isolation, inherited
  environment variables, and access to the server's standard input and output.
- Logs, screenshots, notifications, crash reports, UI dumps, and forensic output
  may contain credentials or personal data. Store, transmit, and retain them as
  sensitive material.
- The server begins continuous screen capture at startup to keep an in-memory
  frame cache for low-latency vision. This consumes device and network resources
  and can observe sensitive screen contents before a screenshot tool is called.
  Use the explicit stream-stop action whenever continuous capture is not
  acceptable, and terminate the server when automation is complete.

Run the server with least privilege, use a dedicated test device and host account
where practical, keep ADB exposure off untrusted networks, and revoke debugging
authorization when it is no longer required.

## Out of scope

Reports need a concrete security impact. General hardening suggestions, feature
requests, and failures that require an operator to intentionally grant the same
authority used by the demonstrated action are normally handled as ordinary
issues. A bypass of a documented opt-in, confinement, device-selection, request
limit, or authorization control remains in scope.
