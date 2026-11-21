# Precision component gallery

The gallery exercises Verifact's shared native design system without adding a production navigation destination. Its rows, statuses and progress are explicitly sample content, and its actions only update local gallery state.

Build the app, then launch a separate development session:

```sh
./scripts/build.sh
VERIFACT_DESIGN_SHOWCASE=1 \
VERIFACT_DATA_DIR="$PWD/.tools/showcase-data" \
VERIFACT_CDP_PORT=9237 \
./scripts/run.sh
```

The separate data directory keeps startup metadata isolated from the normal desktop session. Theme selections in this gallery are transient; persistent product preferences belong to the normal workspace.

The gallery uses `Showcase::new(window, cx)` inside a GPUI Component `Root` after `gpui_component::init`. It consumes `design::Palette`, shared spacing/type constants, and `design::apply`. No browser view is created in gallery mode.

## Interaction checks

- Click **Increment demo** and confirm the counter increases by one. A disabled button must not increment it.
- Click **Advance progress** to cycle through 25% steps. The label explicitly distinguishes this example from an actual test run.
- Click **Toggle loading**, inspect the shared loading spinner, and confirm the loading example cannot increment the counter while loading. Toggle it off to restore interaction.
- Type into **Sample label** and verify the live echo. Inspect Tab focus, selection and normal editing shortcuts. The disabled field must remain unchanged.
- Switch between **Dark**, **Light** and **System** without restarting. The system option follows native appearance changes while selected.
- Search the 1,000 generated filenames. `0042` should produce exactly one matching sample row. Select it, clear the query, and scroll the virtualised list.
- Open **Inspect component contract**, then dismiss the popover with Escape or an outside click.
- Click **Reset demo** to clear counter, progress, loading, inputs and sample selection.
- Resize the window and inspect wrapping, focus contrast, metadata readability and scrolling in both themes.

The list uses GPUI `uniform_list` to render only visible sample rows. List selection is mouse-driven in this initial gallery; mature input and button keyboard behavior comes from GPUI Component. Full screen-reader/accessibility support is not claimed.

## Foundation details

Buttons, inputs, progress, popovers and loading behavior come from the single GPUI Component foundation. Button label children use `design::action_label` because upstream GPUI Component 0.5.1 hardcodes a red hover foreground on the parent button; explicit semantic foregrounds preserve Precision's palette without replacing the library's interactions. Disabled example text uses the disabled semantic role.

This page describes available checks, not a claim that native visual acceptance has run. The implementation-status document records the actual executed verification.
