# Contributing

Thank you for improving droidsight. Keep changes focused, reviewable, and safe
to exercise without an Android device by default.

Participation is governed by [CODE_OF_CONDUCT.md](CODE_OF_CONDUCT.md).
Suspected vulnerabilities go through the private process in
[SECURITY.md](SECURITY.md), not a public issue or pull request.

## Project scope

Product changes belong in the Rust sources, tests, and operational
documentation. Do not commit device captures, screenshots, credentials,
generated binaries, compiler logs, or editor state.

Keep experimental APIs out of the published MCP tool surface unless their
behavior, authority, failure modes, and test coverage are defined.

Changes should preserve these boundaries unless the proposal explicitly and
carefully changes them:

- device selection must fail safely when the target is ambiguous;
- arbitrary shell execution remains opt-in, and no other tool is a way around
  it;
- host file access remains confined to configured roots;
- external commands have deadlines and owned cleanup;
- MCP and subprocess output is bounded where it can be large;
- stderr is used for diagnostics so stdout remains valid MCP transport; and
- ordinary tests do not require or mutate a connected Android device.

## Reaching the device shell

`adb shell` does not preserve argument boundaries. It joins everything after
`shell` with spaces and hands one string to the device's shell, so an unquoted
value escapes its argument exactly as it would in `sh -c`. A tool that passes a
caller-supplied package name or path through unquoted is a general-purpose
device shell wearing another name, which would make the `DROIDSIGHT_ALLOW_SHELL`
gate meaningless.

There are two ways to run something on the device, and the choice is the whole
of the defense:

- `Adb::shell(&["shell", program, arg, ...])` quotes every element after
  `shell`. **Use this.** Anything a caller supplied stays one literal word no
  matter what it contains.
- `Adb::device_shell(command)` evaluates the string verbatim, so pipelines,
  redirection, and `;` work. It exists for commands that genuinely need shell
  syntax and for the opt-in `run_shell` and `run_macro` tools. Any value
  interpolated into it that did not come from this crate must be wrapped in
  `adb::shell_quote` first.

## Secret and personal-data checks

The ignore file is deliberately conservative, but it is not a security
boundary: `git add -f` can bypass it and a file that was committed once remains
in history. Install the staged secret hook before making commits:

```sh
pre-commit install
pre-commit run --all-files
```

Before committing, inspect `git diff --cached --name-status` and the staged
diff. Never commit real device captures, browser exports, cookies, tokens,
credentials, personal logs, or unredacted screenshots. Use synthetic fixtures
when a test needs an example value. CI repeats the Gitleaks history scan, so a
finding must be removed and rotated rather than hidden with an ignore rule.

## Development checks

The pinned compiler, formatter, and linter are declared in
`rust-toolchain.toml`. Install rustup, then run all required host-side gates from
the repository root:

```sh
cargo fmt --all -- --check
cargo clippy --locked --all-targets -- -D warnings
cargo test --locked -- --test-threads=1
cargo build --locked --release --bin droidsight -j 1
```

The single test thread prevents interference between tests that temporarily
change process-wide ADB configuration. The release build uses one compilation
job so the same command remains reliable on memory-constrained hosts. CI runs
the same four gates on Linux, macOS, and Windows, without the `-j 1` cap.

A separate job type-checks the crate against the `rust-version` floor declared
in `Cargo.toml`, which is several releases older than the pinned compiler. That
floor is the compatibility promise the crate metadata makes, so raising it has
to be a deliberate manifest edit rather than something a merged pull request
does by accident. Clippy and rustfmt are not run there: their output legitimately
differs between compiler versions.

Update `Cargo.lock` with an intentional dependency change, and include it in the
same pull request. Do not weaken warnings, remove a safety limit, or add a blanket
lint suppression merely to make a gate pass. Add deterministic regression tests
for behavior changes and exercise error, timeout, cleanup, and malformed-input
paths when relevant.

## Tool schemas

Each tool's `schema()` returns an object that already contains the
`inputSchema` key, and the registry merges those keys into the tool entry it
publishes. A new tool must follow that shape:

```rust
fn schema(&self) -> Value {
    json!({
        "inputSchema": {
            "type": "object",
            "properties": { /* ... */ },
            "required": ["action"]
        }
    })
}
```

Returning the inner object alone, or wrapping the result in a second
`inputSchema`, produces an entry whose `inputSchema` has no `type` and which
strict clients reject. Confirm the published shape rather than assuming it:

```sh
printf '%s\n' \
  '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}' \
  '{"jsonrpc":"2.0","method":"notifications/initialized"}' \
  '{"jsonrpc":"2.0","id":2,"method":"tools/list","params":{}}' \
  | cargo run --quiet --bin droidsight | tail -n 1 | jq '.result.tools[0]'
```

Each entry must have `name`, `description`, and an `inputSchema` whose own
`type` is `"object"`.

## Working against a real device

The test suite runs entirely on the host and never needs a phone. If you do
exercise a change against hardware, use a disposable device with no personal
accounts or irreplaceable state, and set `DROIDSIGHT_DEVICE_SERIAL` to the
intended serial rather than relying on whichever device appears first.

Never attach raw logs, screenshots, notification data, UI dumps, crash reports,
device serials, or forensic output to a public pull request without reviewing
and redacting them.

## Pull requests

Describe the user-visible behavior, security or authority implications, tests
performed, and any checks that could not be run. Call out changes to tool schemas,
environment variables, filesystem access, subprocess construction, device
selection, output limits, or protocol lifecycle explicitly.

Keep protocol behavior backward compatible when practical. If compatibility is
not possible, document the migration and avoid mixing it with unrelated cleanup.
Physical-device evidence complements the deterministic host suite; it does not
replace it.
