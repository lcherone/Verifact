//! Candidate-file catalogue built on GPUI Component's virtualized table.
//! Selection, column measurement/resizing and arrow keys remain library-owned.
use crate::design::{self, Palette};
use gpui::{
    App, AppContext, Context, Div, Entity, EventEmitter, FocusHandle, Focusable, FontWeight,
    InteractiveElement, IntoElement, MouseButton, ParentElement, Render, Stateful, Styled,
    Subscription, Window, canvas, div, px,
};
use gpui_component::{
    ActiveTheme, Sizable,
    table::{Column, Table, TableDelegate, TableEvent, TableState},
};
use std::collections::BTreeMap;
use verifact_core::{ScanReport, TestCandidate};

#[derive(Clone, Debug)]
pub struct CandidateSelected {
    pub id: String,
}

struct CandidateRow {
    id: String,
    file: String,
    framework: String,
}

struct CandidateDelegate {
    columns: [Column; 3],
    fallback_column: Column,
    rows: Vec<CandidateRow>,
}

impl CandidateDelegate {
    fn new() -> Self {
        Self {
            columns: [
                Column::new("file", "File").width(px(480.)).movable(false),
                Column::new("framework", "Framework")
                    .width(px(120.))
                    .movable(false),
                Column::new("status", "Status")
                    .width(px(90.))
                    .movable(false),
            ],
            fallback_column: Column::new("unknown", "").width(px(0.)),
            rows: Vec::new(),
        }
    }
}

impl TableDelegate for CandidateDelegate {
    fn columns_count(&self, _: &App) -> usize {
        self.columns.len()
    }

    fn rows_count(&self, _: &App) -> usize {
        self.rows.len()
    }

    fn column(&self, index: usize, _: &App) -> &Column {
        self.columns.get(index).unwrap_or(&self.fallback_column)
    }

    fn render_header(
        &mut self,
        _: &mut Window,
        cx: &mut Context<TableState<Self>>,
    ) -> Stateful<Div> {
        let palette = Palette::for_dark(cx.theme().is_dark());
        div()
            .id("candidate-table-header")
            .bg(palette.raised)
            .text_color(palette.secondary)
            .text_size(px(design::META_SIZE))
            .font_weight(FontWeight::SEMIBOLD)
    }

    fn render_td(
        &mut self,
        row_index: usize,
        column_index: usize,
        _: &mut Window,
        cx: &mut Context<TableState<Self>>,
    ) -> impl IntoElement {
        let palette = Palette::for_dark(cx.theme().is_dark());
        let Some(row) = self.rows.get(row_index) else {
            return div();
        };
        let (text, color, font) = match column_index {
            0 => (row.file.as_str(), palette.text, design::FONT_CODE),
            1 => (row.framework.as_str(), palette.secondary, design::FONT_UI),
            2 => ("Not run", palette.secondary, design::FONT_UI),
            _ => ("", palette.secondary, design::FONT_UI),
        };
        div()
            .size_full()
            .flex()
            .items_center()
            .overflow_hidden()
            .whitespace_nowrap()
            .font_family(font)
            .text_size(px(design::META_SIZE))
            .text_color(color)
            .child(text.to_owned())
    }

    fn render_empty(
        &mut self,
        _: &mut Window,
        cx: &mut Context<TableState<Self>>,
    ) -> impl IntoElement {
        let palette = Palette::for_dark(cx.theme().is_dark());
        div()
            .size_full()
            .flex()
            .flex_col()
            .justify_center()
            .items_center()
            .gap_2()
            .p_6()
            .text_size(px(design::BODY_SIZE))
            .text_color(palette.secondary)
            .child("No matching candidate files")
            .child(
                div()
                    .text_size(px(design::META_SIZE))
                    .child("Adjust the filters or scan the project again."),
            )
    }
}

pub struct CandidateTable {
    table: Entity<TableState<CandidateDelegate>>,
    tab_focus: FocusHandle,
    automatic_file_width: bool,
    _subscriptions: Vec<Subscription>,
}

impl EventEmitter<CandidateSelected> for CandidateTable {}

impl Focusable for CandidateTable {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.tab_focus.clone()
    }
}

impl CandidateTable {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let table = cx.new(|cx| {
            TableState::new(CandidateDelegate::new(), window, cx)
                .sortable(false)
                .col_movable(false)
                .col_selectable(false)
                .col_resizable(true)
                .row_selectable(true)
                .loop_selection(false)
        });
        let tab_focus = cx.focus_handle();
        let events = cx.subscribe(&table, |this, table, event: &TableEvent, cx| match event {
            TableEvent::SelectRow(index) => {
                if let Some(id) = table
                    .read(cx)
                    .delegate()
                    .rows
                    .get(*index)
                    .map(|row| row.id.clone())
                {
                    cx.emit(CandidateSelected { id });
                }
            }
            TableEvent::ColumnWidthsChanged(widths) => {
                this.automatic_file_width = false;
                // Refresh recreates the library's measured column groups. Retain
                // the user's widths in the delegate before a later filter change.
                table.update(cx, |state, _| {
                    for (column, width) in state.delegate_mut().columns.iter_mut().zip(widths) {
                        column.width = *width;
                    }
                });
            }
            _ => {}
        });
        let focus = cx.on_focus(&tab_focus, window, |this, window, cx| {
            this.table.focus_handle(cx).focus(window);
        });
        let table_focus = table.focus_handle(cx);
        let focus_in = cx.on_focus(&table_focus, window, |_, _, cx| cx.notify());
        let focus_out = cx.on_blur(&table_focus, window, |_, _, cx| cx.notify());
        Self {
            table,
            tab_focus,
            automatic_file_width: true,
            _subscriptions: vec![events, focus, focus_in, focus_out],
        }
    }

    /// Update on scan/filter changes, not during rendering. Selection is retained
    /// by candidate identity rather than the old row index after filtering.
    pub fn set_entries(
        &mut self,
        report: Option<&ScanReport>,
        candidates: Vec<TestCandidate>,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let selected_id = {
            let table = self.table.read(cx);
            table
                .selected_row()
                .and_then(|index| table.delegate().rows.get(index))
                .map(|row| row.id.clone())
        };
        let framework_names: BTreeMap<_, _> = report
            .into_iter()
            .flat_map(|report| &report.frameworks)
            .map(|framework| (framework.id.as_str(), framework.name.as_str()))
            .collect();
        let rows: Vec<_> = candidates
            .into_iter()
            .map(|candidate| CandidateRow {
                id: candidate.id,
                file: candidate.file.to_string_lossy().into_owned(),
                framework: framework_names
                    .get(candidate.framework_id.as_str())
                    .copied()
                    .unwrap_or("Unknown")
                    .to_owned(),
            })
            .collect();
        let selected_index = selected_id
            .as_ref()
            .and_then(|id| rows.iter().position(|row| &row.id == id));
        self.table.update(cx, |table, cx| {
            table.delegate_mut().rows = rows;
            table.clear_selection(cx);
            table.refresh(cx);
            if let Some(index) = selected_index {
                table.set_selected_row(index, cx);
            }
        });
        cx.notify();
    }
}

impl Render for CandidateTable {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let palette = Palette::for_dark(cx.theme().is_dark());
        let table_focus = self.table.focus_handle(cx);
        let focused = table_focus.is_focused(window);
        let view = cx.entity();
        div()
            .id("candidate-catalogue")
            .relative()
            .size_full()
            .min_w_0()
            .min_h_0()
            .track_focus(&self.tab_focus)
            .tab_index(0)
            .border_1()
            .border_color(if focused {
                palette.focus
            } else {
                palette.border
            })
            .rounded(px(design::RADIUS))
            .overflow_hidden()
            .bg(palette.surface)
            // Upstream Table supplies its own focus context and arrow actions;
            // transfer native mouse focus there without reimplementing keys.
            .on_mouse_down(MouseButton::Left, move |_, window, _| {
                table_focus.focus(window);
            })
            .child(Table::new(&self.table).bordered(false).large())
            .child(
                canvas(
                    move |bounds, _, cx| {
                        view.update(cx, |this, cx| {
                            if !this.automatic_file_width {
                                return;
                            }
                            this.table.update(cx, |state, cx| {
                                let columns = &mut state.delegate_mut().columns;
                                // The library supports pixel widths. Adapt File
                                // to the available panel until the user resizes
                                // a column; keep room for its trailing spacer.
                                let width = (bounds.size.width
                                    - columns[1].width
                                    - columns[2].width
                                    - px(14.))
                                .max(px(180.));
                                if (columns[0].width - width).abs() > px(0.5) {
                                    columns[0].width = width;
                                    state.refresh(cx);
                                }
                            });
                        });
                    },
                    |_, _, _, _| {},
                )
                .absolute()
                .size_full(),
            )
    }
}
