# Verifact architecture

## Shared core and native workspace

`crates/verifact-core` owns project identity, passive discovery, SQLite persistence and scan job coordination. `crates/verifact-cli` uses this core directly without GPUI or CEF. The native workspace uses the same service and persistence model; scans run away from the UI thread and generation-tagged results prevent stale metadata replacing a newer selection.

Discovery reads manifests, configuration and candidate source files without executing project code. Candidate files are not individual runner-enumerated test cases. Nested package ownership, bounded reads/traversal, symlink exclusion, stable identities and visible warnings keep passive import deterministic. A narrow Vision Loop adapter recognizes `.vscode/vision-loop/package.json`, records only direct configuration paths, and parses suite envelopes into file candidates without retaining or executing their target/action values. It does not infer Playwright Test from the harness dependency. Scans append immutable snapshots. Snapshot persistence and job finalization are separate transactions; they are not claimed atomic together.

`src/workspace.rs` implements projects, catalogue/filtering, file inspection, scan details, preferences and browser navigation. `src/catalogue.rs` uses GPUI Component’s virtualized table. `src/design.rs` maps semantic Precision tokens to GPUI Component themes; assets are embedded. A developer-only showcase demonstrates component states without adding speculative production navigation.

## Native Chromium host

GPUI owns the AppKit application, native window, GPU-rendered controls and event loop. `src/browser_host.rs` creates a native child NSView for CEF Chromium. Chromium paints and receives Cocoa input directly. The browser is created only when opened; project workspace startup does not initialize a Chromium session or CDP listener.

A Cocoa subclass extends GPUI’s internal `GPUIApplication`, preserving its ivar and delegate while implementing CEF event-state protocols. This internal class dependency must be revalidated when GPUI changes. A foreground task pumps CEF’s external message loop at an 8 ms interval while the runtime exists.

Browser close is asynchronous. The adapter waits for CEF close approval and `on_before_close`, detaches/releases native views and browser references, then shuts down CEF before allowing Cocoa process termination. cef-rs utilities package the framework and helper bundles.

The Node verification worker calls `chromium.connectOverCDP`, selects the context/page created by this host and uses standard Playwright APIs. It does not call `launch` or `newPage`.

## Browser storage and Keychain

Debug builds allocate fresh absolute private `0700` profile directories under the application data directory’s `browser-sessions`. Only these disposable profiles enable Chromium’s testing-keychain switch. No existing Chromium profile is migrated or opened with that switch. Runtime ownership retains the directory until CEF shutdown, then normal cleanup removes it. Forced termination can leave an unused directory; later sessions do not reuse it.

Release builds use `browser-profile` and the dedicated **Verifact Safe Storage** Keychain service/account. They do not enable the testing keychain. Stable application signing and persistent release authentication remain separate unverified work. The current ad-hoc development signature may not preserve macOS access approval across rebuilds.

Inherited command-line switches are disabled; the application callback supplies its intended switches. Development CEF sandboxing remains disabled and CDP binds loopback port 9227 while the browser is open. This is a feasibility/development host, not a production security configuration.

## Hosting decision and remaining gates

The native CEF child-view route demonstrates an interactive in-window browser controllable by Playwright on the tested Mac. WKWebView would leave same-page Playwright attachment unresolved; a separate browser would not satisfy the embedded-browser requirement. Offscreen CEF could integrate through GPUI textures but would require additional input, IME, accessibility and GPU lifetime work.

Keep platform hosting behind this adapter. Before production recording depends on it, complete signing/notarization, sandboxing, crash recovery, long-duration/idle-resource measurements, popup/download policy, complex focus/IME/accessibility and representative application testing. Windows/Linux adapters and the recording system are not implemented.

## Primary references

- [GPUI 0.2.2](https://docs.rs/gpui/0.2.2/gpui/)
- [GPUI Component](https://github.com/longbridge/gpui-component)
- [cef-rs integration and bundling](https://github.com/tauri-apps/cef-rs)
- [CEF application integration](https://bitbucket.org/chromiumembedded/cef/wiki/GeneralUsage)
- [Playwright CDP attachment](https://playwright.dev/docs/api/class-browsertype#browser-type-connect-over-cdp)

Implementation was also checked against installed crate sources and CEF’s macOS protocol/settings headers. See [IMPLEMENTATION_STATUS.md](IMPLEMENTATION_STATUS.md) for evidence boundaries.
