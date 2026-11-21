# Testing Verifact

Build the app and headless CLI:

```sh
npm ci
./scripts/build.sh
. ./scripts/env.sh
cargo fmt --all -- --check
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
npm run verify:core
```

The headless acceptance harness uses isolated temporary storage and separate CLI processes. It checks fourteen fixture projects, deterministic candidate IDs, persisted registry/catalogue/history/jobs, passive scanning and invalid input behavior. Its receipt is `evidence/core-verification.json`.

Close any interactive Verifact and fixture server before browser verification:

```sh
VERIFACT_DATA_DIR="$PWD/.tools/browser-regression-data" npm run verify:browser
```

The browser harness owns fixture port 4179 and launches the native bundled app three times with a 60-second watchdog per process. It attaches Playwright to the existing CEF page over loopback port 9227, without launching a replacement browser. It checks forms, locators, keyboard input, navigation, origin storage, scrolling, screenshots, DOM/accessibility snapshots, console/network capture, traces and clean CEF shutdown. `evidence/verification.json` records actual pass/fail outcomes. Browser-page screenshots do not independently establish native GPUI rendering.

## Native workspace checks

1. Run `VERIFACT_DATA_DIR="$PWD/.tools/desktop-acceptance" ./scripts/run.sh`. Confirm workspace boot does not request a Keychain password.
2. Add `fixtures/discovery/mixed` through the native picker (Cmd+O). Expect two candidate files, PHPUnit and Vitest, all “Not run”.
3. Exercise search (Cmd+F), framework filters, table keyboard selection and column resizing. Inspect a file and scan details, then rescan and check history.
4. Switch Dark, Light and Auto appearances; restart and verify persistence.
5. Start `npm run fixture`, choose Open browser and confirm the fixture renders without a Keychain password prompt. Use native text input, form submission, navigation, scrolling and window resizing.
6. Switch to a project and back to the browser. Confirm the browser retains its session state during this application run.
7. Close the native window. Confirm `CEF_BROWSER_CLOSED` and `CEF_SHUTDOWN_COMPLETE`, port 9227 release and removal of the temporary session directory. Restart and confirm browser login/storage state resets.

Debug profiles use Chromium’s testing keychain only for fresh private disposable directories. Existing protected profiles are not opened with that setting. Never enter real personal credentials in the development browser. A forced termination can leave an unused temporary directory; later launches do not reuse it. Persistent release authentication requires separate signing and packaging verification.

## Design showcase

Run `VERIFACT_DESIGN_SHOWCASE=1 ./scripts/run.sh` to exercise illustrative component states, inputs, counters, popovers, theme variants and the virtualized table. This developer view is not a production navigation item and does not start Chromium. See [SHOWCASE.md](SHOWCASE.md).

See [IMPLEMENTATION_STATUS.md](IMPLEMENTATION_STATUS.md) for executed checks and remaining unverified behavior. Native desktop automation is not human acceptance.

## Adapter coverage

Seven synthetic Vision Loop tests cover narrow metadata discovery, malformed/oversized metadata, private-value exclusion, symlinks and shared scan budgets. Core configuration tests cover atomic persistence, invalid URLs, credentials rejection and re-registration/rescan preservation.
