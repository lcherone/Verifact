# Precision — Verifact's native design system

Precision is a dark-first, restrained visual language for Verifact. The product name remains **Verifact**; Precision names its design language. The foundation lives in `src/design.rs`, and the original product artwork lives in `assets/brand/`.

The design system supports implemented workflows rather than prescribing navigation. Shared surface, spacing, control and type roles can be used in a toolbar, list, inspector or panel without creating new destinations. Future product capabilities do not get placeholder links or screens.

## Architecture and component foundation

The existing GPUI 0.2.2 shell hosts a real CEF Chromium native child view, with Playwright control of that page. Core project registration, passive discovery and SQLite persistence are separated from GPUI in `verifact-core`. Precision changes the surrounding native presentation; it does not replace browser hosting, mutate website CSS, or move core behavior into views.

**Selected:** exact `gpui-component = 0.5.1`, plus its `gpui-component-assets = 0.5.1` companion. Its published manifest depends on GPUI 0.2.2, matching Verifact's pinned framework. The component crate declares Apache-2.0, and its bundled `LICENSE-APACHE` was inspected. The companion supplies the library's Lucide glyph paths. No competing component toolkit or GPUI replacement is introduced.

The selected library supplies actual Button/Input/Root behavior, tab stops, focus handles, keyboard bindings, disabled handling, loading, tooltips, overlays and virtualized List/Table primitives. Precision restyles its semantic theme instead of reimplementing these interactions. `gpui_component::init(cx)` must run before `design::apply(...)`; a component `Root` must own each component window's overlay context. These contracts were checked against the installed 0.5.1 source. Comparative frame-time, startup-time and memory benchmarks have not been run.

### Ecosystem review, 21 November 2026

| Official project | Compatibility, maintenance and decision |
| --- | --- |
| [Longbridge GPUI Kit](https://github.com/longbridge/gpui-kit), [current workspace manifest](https://github.com/longbridge/gpui-kit/blob/main/Cargo.toml) | Active upstream; repository metadata reported a push on 21 November. Current main is the 0.10 series and pins `gpui-pre = 0.3.12`, so adopting current main would change Verifact's framework/API assumptions. The historical published component 0.5.1 is selected instead. Its explicit theme and control APIs meet the current workflow without changing the validated browser dependency. Component package license: Apache-2.0. |
| [GPUI CE component fork](https://github.com/gpui-ce/gpui-component), [workspace manifest](https://github.com/gpui-ce/gpui-component/blob/main/Cargo.toml) | Active fork; metadata reported a push on 27 October. Its current workspace uses renamed CE packages and pins `gpui-ce = 0.3.0`. Component workspace license declares Apache-2.0. This is a framework fork/migration rather than a drop-in layer for pinned GPUI 0.2.2; not selected for this increment. |
| [Guise](https://github.com/wess/guise), [workspace](https://github.com/wess/guise/blob/main/Cargo.toml), [library manifest](https://github.com/wess/guise/blob/main/crates/guise/Cargo.toml) | Maintained alternative: 1.11.4 workspace, GPUI 0.2.2, MIT; metadata reported a push on 3 November. It offers theme tokens, component and reactive-state APIs, and embedded Lucide icons. With default WebView disabled, its smaller direct dependency set is attractive. Adopting its state/component contracts alongside the selected foundation would duplicate behavior; not added. No claim that it is technically incompatible. |
| [Awesome GPUI](https://github.com/zed-industries/awesome-gpui) | Maintained discovery catalogue, not a runtime component library; metadata reported a push on 21 November. CC0-1.0. Useful for finding established GPUI patterns, but inclusion is not compatibility, accessibility or production-readiness proof. |

Research relied on these official repositories, published crate manifests and installed source, not secondary rankings. Maintenance dates are an inspection snapshot, not a support guarantee. The exact selected versions remain in Cargo manifests/lockfile; later upstream API and license changes require a new review.

## Semantic palette

`Palette::for_dark(bool)` returns strongly typed `gpui::Hsla` roles. Views must use those roles rather than local hex literals.

| Role | Dark | Light | Use |
| --- | --- | --- | --- |
| `canvas` | `#121A27` | `#F2F5F8` | Window/workspace canvas |
| `surface` | `#1D2836` | `#FFFFFF` | Main content and panel surfaces |
| `raised` | `#263445` | `#F7F9FC` | Popovers, secondary controls, table headers |
| `border` | `#334155` | `#CDD6E0` | Quiet decorative separators |
| `border_strong` | `#647B96` | `#71839A` | Input boundaries and interactive outlines |
| `text` | `#F1F5F9` | `#172536` | Main labels and content |
| `secondary` | `#B4C0D0` | `#465970` | Supporting descriptions |
| `muted` | `#94A3B8` | `#5A6D82` | Ordinary metadata, not disabled text |
| `accent` / `focus` | `#43D9B2` | `#087F67` | Primary actions, links and focus indication |
| `accent_text` | `#10251F` | `#FFFFFF` | Foreground on primary fills |
| `selected` | `#1C3D35` | `#E0F3EC` | Selected row/control background |
| `hover` | `#2B3A4D` | `#EAF0F5` | Neutral interaction feedback |
| `success` | `#62DBAE` | `#127547` | Confirmed success only |
| `failure` | `#FF8B99` | `#B4233B` | Failure/error emphasis |
| `warning` | `#E6BE70` | `#8F5A0C` | Warning/pending emphasis |
| `info` | `#82B7FF` | `#245DBB` | Information and active work |
| `disabled` | `#71839B` | `#8896A7` | Disabled foreground only |
| `disabled_surface` | `#1A2533` | `#EDF1F5` | Inactive control/skeleton background |

`design::apply` maps these roles into component surfaces, inputs, buttons, selection, tables, lists, tabs, sidebar, popovers, focus rings, progress, feedback and chart colors. Hover/active accent fills change lightness centrally. Theme-dependent code highlighting remains owned by the component library; no separate editor is introduced.

Table row selection uses a 12% accent overlay with the focus border. GPUI Component paints this layer above cell contents; keep it translucent so selected filenames, frameworks and statuses stay readable in both themes. The opaque `selected` surface remains appropriate for components that paint their selection behind content.

Use explicit text labels with status colors. **Not run**, **Unknown**, **Skipped** and **Cancelled** are neutral; none imply success. **Running** uses information emphasis, and **Queued** may use warning emphasis. Confirmation comes from actual execution evidence, never the presence of green UI. Failure treatment should identify the failing item and useful next action before revealing logs.

### Contrast checks

The numeric checks in `design.rs` use WCAG relative sRGB luminance and contrast ratios. They cover every pair of primary/secondary/muted text against canvas/surface/raised/hover/selected in both themes, primary control text in default/hover/active states, and input/focus boundaries. Ordinary text targets at least **4.5:1**, and interactive boundaries/focus target at least **3:1** against the primary surface.

Representative analytic ratios:

| Pair | Dark | Light |
| --- | --- | --- |
| Muted text on raised surface | 4.94:1 | 5.05:1 |
| Muted text on hover surface | 4.51:1 | 4.63:1 |
| Primary button foreground/fill | 9.04:1 | 4.95:1 |
| Strong input border on primary surface | 3.42:1 | 3.88:1 |

Quiet `border` separators and disabled controls are not substituted for interactive boundaries or ordinary readable text. These are token-level numeric checks, not a full accessibility audit: compositing, font rasterization, actual focus rings, hover/disabled appearance and rendered layouts still require native verification.

## Typography, dimensions and motion

Use `.SystemUIFont` for native UI text and `Menlo` for source paths, identifiers, commands and technical output on the current macOS target. System fonts are referenced by name, not redistributed. Monospaced technical durations and counts naturally keep digits aligned. Do not add bundled fonts without checking their license and packaging.

| Token | Value |
| --- | --- |
| Body / metadata / section / workspace title | 13 / 12 / 14 / 22 px |
| `SPACE_1/2/3/4/6/8` | 4 / 8 / 12 / 16 / 24 / 32 px |
| Default control height / icon size | 32 / 16 px |
| Control radius / outer gutter | 6 / 24 px |
| Optional project panel width | 232 px |
| Browser-compatible header reservation | 96 px |

These dimensions are reusable defaults, not mandatory screen architecture. Use compact real labels and progressive disclosure rather than shrinking readable text. Keep the embedded page as the visual focus when browsing. The 96 px header token preserves the existing browser's layout reservation; the hosting adapter owns any native-view geometry updates.

No decorative motion is added by the theme or brand. Use animation only to explain actual work or state changes, and never delay actions for a transition. The current foundation does not claim to implement an OS reduced-motion subscription; keep future animation opt-in and provide a reduced-motion path before adding it.

## Theme preference contract

`ThemePreference` has `Dark`, `Light` and `System` variants, stable lowercase storage strings, and explicit resolution of all four GPUI window appearances. Unknown stored strings return the dark-first default.

The application owns persistence and subscribes to system appearance changes. `design::apply(preference, window, cx)` resolves the current appearance, updates the library theme and Precision roles, and refreshes native windows. It does not write settings or install its own competing appearance observer. Both palette definitions are deliberate; light mode is not a numeric inversion of dark mode.

## One precise upstream style workaround

The installed `gpui-component` 0.5.1 `src/button/button.rs` sets a hardcoded `red_400()` foreground in its hover closure (line 518 in the inspected crate), bypassing themed foreground roles. Verifact does not fork the dependency, upgrade GPUI, or reimplement its button behavior to work around this.

Use `Button.child(design::action_label(text, &palette, primary))`: the helper returns an owned `gpui::Div` with an explicit Precision foreground, preventing the label from inheriting the parent's red hover color. The original Button still owns focus, keyboard activation, hit-testing, click suppression, disabled/loading state and tooltip behavior. A concrete return type also avoids Rust 2024 opaque-return lifetime capture of the borrowed palette.

An explicitly colored child also bypasses the parent's disabled foreground. For a disabled action, retain `.disabled(true)` and color that child with `palette.disabled` where needed. Do not replace activation suppression with an inert click handler. Status/destructive variants should use their corresponding foreground role rather than blindly treating every accent-filled button as primary.

## Original identity and asset conventions

The chosen mark is an **evidence aperture**: four registration corners frame a diamond focal plane and a single sample point. It suggests observation, precision and one grounded fact. A stacked-document concept was considered but would be less distinct at native toolbar sizes. No external product mark was copied, and no checkmark or robot motif was used.

- `assets/brand/verifact-mark.svg`: regular dark-surface mark.
- `assets/brand/verifact-mark-light.svg`: light-surface accent treatment.
- `assets/brand/verifact-mark-small.svg`: simplified 16–20 px optical treatment.
- `assets/brand/verifact-app-icon.svg`: editable app-icon master.
- `assets/brand/verifact-app-icon.png`: deterministic 1024 px rendering.
- `assets/brand/Verifact.icns`: macOS multi-resolution icon.

Keep the mark upright, with clear space of at least one eighth of its width. The wordmark is live **Verifact** text in the native UI font beside the symbol; the brand graphic does not bake in product text or impose a font dependency. Use `Verifact` in human-facing labels and `verifact` in package/executable identifiers. Use the same logo and semantic palette in documentation without flooding every surface with branding.

Original SVG geometry has no third-party artwork or font dependency. See `assets/README.md` for provenance. Application glyphs come from the single Lucide-based component asset set; do not mix emoji, arbitrary Unicode arrows and unrelated icon styles for action affordances. An icon supplements its text/tooltip and is never the only status cue.

The PNG and ICNS were generated from the SVG with CairoSVG 2.9.1 and macOS `iconutil`; no image-generation service is needed for these code-native vector assets. To regenerate using the workspace's optional Python tooling:

```sh
DYLD_FALLBACK_LIBRARY_PATH=/opt/homebrew/lib .tools/build-tools/bin/python assets/brand/build_icons.py
```

The source vector and packaged icon were inspected as artifacts. This does not prove that the running Dock has refreshed its icon cache; bundle integration and rendered application verification remain separate evidence.

## Foundation checks executed

- `cargo test --bin verifact design::tests -- --nocapture`: **2 passed**, covering stored preference/system appearance resolution and the full text/control contrast matrix.
- Direct `rustfmt --edition 2024 --check src/design.rs`: **passed**.
- `cargo clippy --bin verifact -- -D warnings`: **passed** for the integrated native binary at this point. Dependency tooling still emits the existing future-Rust compatibility notice for `block` and `proc-macro-error2`.
- All four SVG files parsed successfully as XML with explicit view boxes.
- `iconutil` decoded the generated ICNS into **10 PNG representations**; each expected 1x/2x dimension was checked. The 1024 px PNG was visually inspected as an artifact.

These checks validate the foundation and artwork. They do not substitute for root-owned running application checks.

## Accessibility and verification boundaries

GPUI Component's existing focus and input behavior should be retained. Icon actions need descriptive tooltips and visible/keyboard-discoverable context; keyboard focus must have a visible ring. Tables/lists should use the selected library's virtualization when collections grow rather than materializing an unbounded number of rows.

The installed pinned GPUI 0.2.2 and component 0.5.1 manifests do not include AccessKit, and this source inspection did not establish complete native semantic roles or screen-reader announcements. Keyboard support is not screen-reader proof. Native VoiceOver/other assistive technology, text scaling, reduced-motion changes, all platforms and every interaction variant have **not** been comprehensively audited by this foundation task.

Root integration owns the running native light/dark/system checks, component playground, native screenshots, window resizing and browser regression tests. A passing theme unit test cannot substitute for those checks. No production navigation entry is needed for the development playground, and no future recording or AI feature is represented as implemented by these tokens.
