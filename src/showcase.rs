//! Development-only Precision component gallery. Every displayed status and row
//! is sample content; actions update local gallery state, never project results.
use crate::design::{
    self, BODY_SIZE, CONTROL_HEIGHT, FONT_CODE, FONT_UI, GUTTER, META_SIZE, Palette, RADIUS,
    SECTION_SIZE, SPACE_1, SPACE_2, SPACE_3, SPACE_4, SPACE_6, TITLE_SIZE, ThemePreference,
};
use gpui::{
    ClickEvent, Context, Div, Entity, Hsla, Render, SharedString, Subscription, Window, div,
    prelude::*, px, uniform_list,
};
use gpui_component::{
    ActiveTheme, Disableable, Selectable,
    button::{Button, ButtonVariants},
    input::{Input, InputEvent, InputState},
    popover::Popover,
    progress::Progress,
};

pub struct Showcase {
    preference: ThemePreference,
    name: Entity<InputState>,
    search: Entity<InputState>,
    disabled_input: Entity<InputState>,
    clicks: usize,
    progress: f32,
    loading: bool,
    sample_rows: Vec<SharedString>,
    visible_samples: Vec<usize>,
    selected_sample: Option<usize>,
    _subscriptions: Vec<Subscription>,
}

impl Showcase {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        design::apply(ThemePreference::Dark, Some(window), cx);
        let name = cx.new(|cx| InputState::new(window, cx).placeholder("Type a sample label"));
        let search =
            cx.new(|cx| InputState::new(window, cx).placeholder("Search sample filenames"));
        let disabled_input =
            cx.new(|cx| InputState::new(window, cx).default_value("Disabled example input"));
        let mut subscriptions: Vec<Subscription> = [&name, &search]
            .into_iter()
            .map(|input| {
                cx.subscribe_in(input, window, |_, _, event: &InputEvent, _, cx| {
                    if matches!(event, InputEvent::Change) {
                        cx.notify();
                    }
                })
            })
            .collect();
        subscriptions.push(cx.observe_window_appearance(window, |this, window, cx| {
            design::apply(this.preference, Some(window), cx);
            cx.notify();
        }));
        Self {
            preference: ThemePreference::Dark,
            name,
            search,
            disabled_input,
            clicks: 0,
            progress: 25.,
            loading: false,
            sample_rows: (1..=1000)
                .map(|index| format!("tests/example-{index:04}.test.ts").into())
                .collect(),
            visible_samples: Vec::new(),
            selected_sample: None,
            _subscriptions: subscriptions,
        }
    }

    fn increment(&mut self, _: &ClickEvent, _: &mut Window, cx: &mut Context<Self>) {
        self.clicks += 1;
        cx.notify();
    }

    fn advance_progress(&mut self, _: &ClickEvent, _: &mut Window, cx: &mut Context<Self>) {
        self.progress = if self.progress >= 100. {
            0.
        } else {
            self.progress + 25.
        };
        cx.notify();
    }

    fn toggle_loading(&mut self, _: &ClickEvent, _: &mut Window, cx: &mut Context<Self>) {
        self.loading = !self.loading;
        cx.notify();
    }

    fn reset_demo(&mut self, _: &ClickEvent, window: &mut Window, cx: &mut Context<Self>) {
        self.clicks = 0;
        self.progress = 0.;
        self.loading = false;
        self.selected_sample = None;
        for input in [&self.name, &self.search] {
            input.update(cx, |input, cx| input.set_value("", window, cx));
        }
        cx.notify();
    }

    fn select_sample(&mut self, index: usize, cx: &mut Context<Self>) {
        self.selected_sample = Some(index);
        cx.notify();
    }

    fn theme_button(
        &self,
        label: &'static str,
        preference: ThemePreference,
        cx: &Context<Self>,
    ) -> Button {
        let palette = Palette::for_dark(cx.theme().is_dark());
        Button::new(label)
            .child(design::action_label(label, &palette, false))
            .selected(self.preference.as_str() == preference.as_str())
            .on_click(cx.listener(move |this, _, window, cx| {
                this.preference = preference;
                design::apply(preference, Some(window), cx);
                cx.notify();
            }))
    }
}

fn section(title: &'static str, palette: &Palette) -> Div {
    div().flex().flex_col().gap(px(SPACE_3)).child(
        div()
            .text_size(px(SECTION_SIZE))
            .text_color(palette.text)
            .child(title),
    )
}

fn status(label: &'static str, color: Hsla, palette: &Palette) -> Div {
    div()
        .flex_shrink_0()
        .px(px(SPACE_2))
        .py(px(SPACE_1))
        .rounded(px(RADIUS))
        .border_1()
        .border_color(palette.border)
        .bg(palette.surface)
        .text_size(px(META_SIZE))
        .text_color(color)
        .child(label)
}

impl Render for Showcase {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let palette = Palette::for_dark(cx.theme().is_dark());
        let query = self.search.read(cx).value().to_lowercase();
        self.visible_samples = self
            .sample_rows
            .iter()
            .enumerate()
            .filter_map(|(index, name)| name.to_lowercase().contains(&query).then_some(index))
            .collect();
        let label = self.name.read(cx).value();
        let echo = if label.trim().is_empty() {
            "Your sample label appears here as you type.".to_owned()
        } else {
            format!("Sample label: {label}")
        };
        let selected = self.selected_sample.map_or_else(
            || "Select a sample row to inspect its selection state.".to_owned(),
            |index| format!("Selected sample: {}", self.sample_rows[index]),
        );

        div()
            .size_full()
            .bg(palette.canvas)
            .text_color(palette.text)
            .font_family(FONT_UI)
            .text_size(px(BODY_SIZE))
            .child(
                div()
                    .id("precision-showcase-scroll")
                    .size_full()
                    .overflow_y_scroll()
                    .p(px(GUTTER))
                    .child(
                        div()
                            .max_w(px(1160.))
                            .mx_auto()
                            .flex()
                            .flex_col()
                            .gap(px(SPACE_6))
                            .child(
                                div().flex().items_start().justify_between().gap(px(SPACE_4))
                                    .child(div().flex().flex_col().gap(px(SPACE_2))
                                        .child(div().text_size(px(TITLE_SIZE)).child("Verifact / Precision"))
                                        .child(div().text_color(palette.secondary).child("Development component gallery · sample content only")))
                                    .child(div().flex().gap(px(SPACE_2))
                                        .child(self.theme_button("Dark", ThemePreference::Dark, cx))
                                        .child(self.theme_button("Light", ThemePreference::Light, cx))
                                        .child(self.theme_button("System", ThemePreference::System, cx))),
                            )
                            .child(
                                section("Actions", &palette)
                                    .child(div().text_color(palette.secondary).child("Buttons affect this gallery only. Disabled and loading controls cannot increment the counter."))
                                    .child(div().flex().flex_wrap().items_center().gap(px(SPACE_2))
                                        .child(Button::new("sample-primary").primary().child(design::action_label("Increment demo", &palette, true))
                                            .on_click(cx.listener(Self::increment)))
                                        .child(Button::new("sample-secondary").child(design::action_label("Advance progress", &palette, false))
                                            .on_click(cx.listener(Self::advance_progress)))
                                        .child(Button::new("sample-ghost").ghost().child(design::action_label("Toggle loading", &palette, false))
                                            .on_click(cx.listener(Self::toggle_loading)))
                                        .child(Button::new("sample-danger").danger().child(design::action_label("Reset demo", &palette, false))
                                            .on_click(cx.listener(Self::reset_demo)))
                                        .child(Button::new("sample-disabled").child(div().text_color(palette.disabled).child("Disabled example")).disabled(true)
                                            .on_click(cx.listener(Self::increment)))
                                        .child(Button::new("sample-loading").child(design::action_label("Loading example", &palette, false)).loading(self.loading)
                                            .on_click(cx.listener(Self::increment))))
                                    .child(div().text_color(palette.secondary).child(format!("Demo counter: {}", self.clicks)))
                                    .child(Progress::new().value(self.progress).bg(palette.accent))
                                    .child(div().text_size(px(META_SIZE)).text_color(palette.secondary).child(format!("Example progress: {:.0}% · no test run is being executed", self.progress))),
                            )
                            .child(
                                section("Inputs and focus", &palette)
                                    .child(div().flex().gap(px(SPACE_4)).flex_wrap()
                                        .child(div().w(px(340.)).flex().flex_col().gap(px(SPACE_2))
                                            .child("Sample label")
                                            .child(Input::new(&self.name))
                                            .child(div().text_color(palette.secondary).child(echo)))
                                        .child(div().w(px(340.)).flex().flex_col().gap(px(SPACE_2))
                                            .child("Disabled input")
                                            .child(Input::new(&self.disabled_input).disabled(true))
                                            .child(div().text_color(palette.secondary).child("Use Tab, arrow keys and standard editing shortcuts in the active inputs.")))),
                            )
                            .child(
                                section("Semantic states · illustrative labels", &palette)
                                    .child(div().flex().flex_wrap().gap(px(SPACE_2))
                                        .child(status("Never executed", palette.muted, &palette))
                                        .child(status("Queued", palette.secondary, &palette))
                                        .child(status("Running", palette.info, &palette))
                                        .child(status("Passed", palette.success, &palette))
                                        .child(status("Failed", palette.failure, &palette))
                                        .child(status("Skipped", palette.secondary, &palette))
                                        .child(status("Cancelled", palette.secondary, &palette))
                                        .child(status("Blocked", palette.warning, &palette))
                                        .child(status("Stale verification", palette.warning, &palette))),
                            )
                            .child(
                                section("Typography and tokens", &palette)
                                    .child(div().text_size(px(TITLE_SIZE)).child("Workspace heading"))
                                    .child(div().text_size(px(SECTION_SIZE)).child("Section heading"))
                                    .child("Body copy keeps the next action and its result easy to understand.")
                                    .child(div().text_size(px(META_SIZE)).text_color(palette.secondary).child("Metadata · tabular details should remain readable"))
                                    .child(div().font_family(FONT_CODE).text_color(palette.secondary).child("cargo test -p verifact-core  // illustrative command, not executed"))
                                    .child(div().flex().flex_wrap().gap(px(SPACE_2)).children([
                                        ("Canvas", palette.canvas), ("Surface", palette.surface), ("Raised", palette.raised),
                                        ("Selected", palette.selected), ("Accent", palette.accent), ("Focus", palette.focus),
                                    ].into_iter().map(|(name, color)| div().flex().items_center().gap(px(SPACE_2))
                                        .child(div().size(px(20.)).rounded(px(RADIUS)).bg(color).border_1().border_color(palette.border_strong))
                                        .child(name)))),
                            )
                            .child(
                                section("Virtualised sample list", &palette)
                                    .child(div().text_color(palette.secondary).child("1,000 generated sample filenames. Search filters the gallery; it does not query a project."))
                                    .child(Input::new(&self.search))
                                    .child(div().text_size(px(META_SIZE)).text_color(palette.secondary).child(format!("{} matching sample rows", self.visible_samples.len())))
                                    .child(
                                        uniform_list("precision-sample-list", self.visible_samples.len(), cx.processor(|this, range: std::ops::Range<usize>, _, cx| {
                                            let palette = Palette::for_dark(cx.theme().is_dark());
                                            range.filter_map(|visible_index| this.visible_samples.get(visible_index).copied())
                                                .map(|sample_index| {
                                                    div().id(sample_index).h(px(CONTROL_HEIGHT)).px(px(SPACE_3)).flex().items_center()
                                                        .bg(if this.selected_sample == Some(sample_index) { palette.selected } else { palette.surface })
                                                        .text_color(palette.text).font_family(FONT_CODE).text_size(px(META_SIZE))
                                                        .hover(|style| style.bg(palette.hover)).cursor_pointer()
                                                        .child(this.sample_rows[sample_index].clone())
                                                        .on_click(cx.listener(move |this, _, _, cx| this.select_sample(sample_index, cx)))
                                                }).collect()
                                        }))
                                        .h(px(220.)).w_full().border_1().border_color(palette.border).rounded(px(RADIUS)),
                                    )
                                    .child(div().text_color(palette.secondary).child(selected)),
                            )
                            .child(
                                section("Overlay and disclosure", &palette)
                                    .child(Popover::new("precision-example-popover")
                                        .trigger(Button::new("show-component-contract").child(design::action_label("Inspect component contract", &palette, false)))
                                        .content(|_, _, cx| {
                                            let palette = Palette::for_dark(cx.theme().is_dark());
                                            div().w(px(320.)).flex().flex_col().gap(px(SPACE_2))
                                                .text_color(palette.text)
                                                .child("Precision component contract")
                                                .child(div().text_color(palette.secondary).child("Semantic tokens, native focus and shared interaction behavior. Press Escape or click outside to dismiss."))
                                        })),
                            ),
                    ),
            )
    }
}
