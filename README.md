# Verifact

Verifact is a local testing workspace built with Rust and GPUI. The current implementation includes passive PHP/Node test-file and Vision Loop suite discovery, a persisted project catalogue, a headless CLI, and a real CEF Chromium browser controlled by Playwright. The wider platform described in [`.idea`](.idea), including test execution and recording, remains in development.

The native workspace follows [`.design`](.design), with semantic dark/light/system themes, an original application identity, and GPUI Component controls. See [DESIGN_SYSTEM.md](DESIGN_SYSTEM.md) and [SHOWCASE.md](SHOWCASE.md).

## Build and run (macOS)

```sh
npm ci
./scripts/build.sh
./scripts/run.sh
```

Choose **Add project** (Cmd+O) and select a PHP or Node project. Discovery reads files without running project scripts or runner configuration. Search the catalogue, filter frameworks, inspect file candidates and scan history, or rescan. Candidates are files, not confirmed individual test cases; no tests are executed.

Projects, scan history and appearance preferences persist in `~/Library/Application Support/Verifact`. Set `VERIFACT_DATA_DIR` to use another directory. Chromium runtime logs are written under its `logs/` directory. `VERIFACT_PROJECT=/path/to/project` imports and scans a project at launch.

For the embedded browser fixture, start `npm run fixture`, then choose **Open browser**. The address bar accepts HTTP/HTTPS URLs; `VERIFACT_URL=http://localhost:3000 ./scripts/run.sh` changes the initial URL.

## Development browser sessions and the macOS password prompt

Development builds use a new private temporary browser profile for each application session. Chromium uses its testing keychain only for that disposable profile, so opening Verifact or its browser does not request access to **Chromium Safe Storage** in the login Keychain. The browser initializes only when opened from the workspace.

Browser cookies, logins and local storage survive navigation within that session, but reset when Verifact closes. Normal shutdown removes the temporary profile. A forced termination may leave a directory under `browser-sessions`; those directories are not reused. Existing Chromium profiles and Keychain items are neither modified nor deleted.

Release builds retain a separate protected profile and use **Verifact Safe Storage**. Persistent release authentication still requires stable application signing and packaging; suppressing Keychain access for persistent credentials is not the development workaround. The current bundle has an ad-hoc signature, so macOS access approvals may stop matching after a rebuild.

## Headless CLI

The CLI uses the same core and SQLite store without loading GPUI or Chromium:

```sh
target/debug/verifact-cli --help
target/debug/verifact-cli --data-dir /tmp/verifact-demo add /path/to/project
target/debug/verifact-cli --data-dir /tmp/verifact-demo scan /path/to/project
target/debug/verifact-cli --data-dir /tmp/verifact-demo projects
```

Use the returned project ID to save a display name and site URL:

```sh
target/debug/verifact-cli configure PROJECT_ID --name "My project" --url http://localhost:3000/
```

The native project header then offers **Open site**. Omitted configuration values stay unchanged; names and URLs persist across imports and rescans. Other commands include `project`, `tests`, `history` and `jobs`. Results are JSON.

Recognized Vision Loop harnesses under `.vscode/vision-loop` expose their direct `suites/*.json` files as catalogue candidates. Discovery records configuration paths without loading their values and excludes stored authentication, runs, actions and unrelated editor tools. Suite files remain “Not run”; runner execution is separate work.

## Verification

Close any running Verifact and fixture server before browser verification, which owns ports 4179 and 9227.

```sh
. ./scripts/env.sh
cargo fmt --all -- --check
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
npm run verify
```

`verify:core` checks fixture discovery, persistence and passive scanning through separate CLI processes. `verify:browser` launches the native bundle three times and attaches Playwright to its existing hosted page; it never launches a replacement browser. To isolate its project registry, set `VERIFACT_DATA_DIR="$PWD/.tools/browser-regression-data"` when running browser checks. Receipts and screenshots are local, ignored files under `evidence/`.

## Toolchain and current boundaries

The workspace-local Rust, CMake and Ninja installations are under ignored `.tools/` directories. `scripts/env.sh` uses them when available. Another Mac needs Rust, Xcode Command Line Tools, CMake, Ninja and Node.js 22 or newer. GPUI uses runtime Metal shader compilation. The initial build downloads CEF and Rust dependencies and uses several GB of disk space.

- macOS is the implemented browser adapter; Windows and Linux are untested.
- The development CEF host disables its sandbox and exposes loopback CDP on port 9227 while the browser is open. Use disposable test data and accounts.
- Signing/notarization, production sandboxing, long-duration reliability, crash recovery, popup policy, downloads, IME and native accessibility require further work.
- Recording, replay generation, test execution, AI, MCP and collaboration are not implemented.

See [IMPLEMENTATION_STATUS.md](IMPLEMENTATION_STATUS.md), [TESTING.md](TESTING.md) and [ARCHITECTURE.md](ARCHITECTURE.md) for the implementation and evidence boundaries.
