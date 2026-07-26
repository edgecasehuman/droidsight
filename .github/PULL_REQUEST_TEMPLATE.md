<!--
Suspected vulnerabilities do not belong in a pull request. Use the private
advisory form; see SECURITY.md.
-->

## What this changes

<!-- The user-visible behaviour, in a sentence or two. -->

## Authority and security

<!--
Delete if none apply. Call out explicitly, because these are the parts of the
surface that reviewers check hardest:

- tool schemas, or a new published tool
- environment variables
- host filesystem access or path confinement
- subprocess construction, deadlines, or cleanup
- device selection
- output limits
- protocol lifecycle
-->

None.

## Checks

- [ ] `cargo fmt --all -- --check`
- [ ] `cargo clippy --locked --all-targets -- -D warnings`
- [ ] `cargo test --locked -- --test-threads=1`
- [ ] A test fails without this change, or the change is genuinely untestable
- [ ] `Cargo.lock` is included, if dependencies changed

## Device testing

<!--
Optional — the host suite never needs a phone. If you did exercise this against
hardware, say which device and Android version.

Anything pasted here must be redacted: no serials, account names, notification
content, or unredacted logs.
-->

Not exercised against a device.
