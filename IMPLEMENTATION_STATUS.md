# Verifact implementation status

Local implementation on 21 November 2026. The repository is on `main` with no commits; nothing has been pushed or deployed.

## Implemented

- Rust core shared by the native app and a headless JSON CLI.
- Passive PHP/Node framework detection, nested package ownership, stable candidate IDs and bounded scanning without executing project code.
- SQLite projects, immutable scan snapshots, job history and preferences. Recovery conservatively marks jobs interrupted only when their recorded process owner has demonstrably exited.
- Saved project names and validated HTTP(S) site URLs, with a native Open site control.
- Passive discovery of recognized Vision Loop suites under `.vscode/vision-loop`, excluding stored auth, runs, actions and unrelated editor tools.
- Native project picker, catalogue, search, framework filtering, scan details and file inspection. Catalogue entries are candidate files, not confirmed test cases; every entry remains “Not run”.
- Precision dark/light/system themes, original brand assets, reusable GPUI Component controls and a development-only showcase.
- Real CEF child-view browser, lazy initialization, address navigation and attachment to its existing page through Playwright.
- Debug builds use private disposable browser profiles with Chromium’s testing keychain. Existing profiles and Keychain items remain untouched. Browser logins reset at application close. Normal shutdown removes temporary profiles; forced termination may leave unused directories.

Release profiles use a separate Verifact Keychain service and retain protection. Stable signing, notarization and production sandboxing remain unresolved; persistent release authentication has not been verified.

## Verification

| Check | Result | Evidence |
| --- | --- | --- |
| Native build and bundle | Passed | `scripts/build.sh`; `target/bundle/Verifact.app` |
| Rust formatting | Passed | `cargo fmt --all -- --check` |
| Workspace tests | Passed: 51 | 3 native, 24 core unit, 11 discovery, 6 service and 7 Vision Loop tests |
| Workspace lint | Passed | `cargo clippy --workspace --all-targets -- -D warnings` |
| Headless acceptance | Passed: 7 groups, 29 processes | `evidence/core-verification.json` |
| Embedded Playwright interaction and lifecycle | Passed: 3 launches | `evidence/verification.json`; attach to existing page, form, navigation, storage, evidence capture, shutdown and port release |
| Workspace boot without Keychain prompt | Passed, agent operated | `evidence/keychain-final-boot.log`; native window inspected |
| Browser opening without Keychain prompt | Passed, agent operated | `evidence/keychain-browser-no-prompt.png` |
| Disposable profile cleanup | Passed after native close and three regression cycles | `evidence/keychain-verification.json` |
| Native project picker, persisted catalogue and dark/light rendering | Passed, agent operated | `evidence/precision-catalogue-dark.png`; native workspace inspected |
| Complete showcase and all workspace interactions | Not fully verified | Component checks do not replace end-to-end native acceptance |
| Persistent release browser authentication | Not run | Requires release signing/packaging |
| Human acceptance | Not run | Agent-operated checks are distinct from user acceptance |

Evidence is local and ignored by Git. Browser regression uses a fixture at `127.0.0.1:4179` and CDP on loopback port 9227. Tested environment: macOS 15.6 arm64, Rust 1.100.0, Node.js 22.22.2, GPUI 0.2.2, GPUI Component 0.5.1, cef-rs 156.7.0+156.0.31 and Playwright 1.65.1.

## Remaining platform work

Test execution, recording, replay generation, AI, MCP and collaboration are not implemented. Windows/Linux hosting, production sandboxing/signing, renderer crash recovery, long-duration reliability, popup/download policy, IME and native accessibility remain unverified or incomplete. GPUI internal application-class integration and fixed-interval CEF pumping require review before production use or dependency upgrades.

Current dependencies report future Rust compatibility notices for `block` and `proc-macro-error2`; current tests and lint pass. See [README.md](README.md) for running the implemented slice and [TESTING.md](TESTING.md) for reproduction.
