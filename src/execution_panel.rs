//! Native presentation of the shared execution service. All database/process
//! operations run on workers; approval is explicit and scoped to a prepared plan.
use crate::design::{self, Palette};
use gpui::{prelude::*, *};
use gpui_component::{
    ActiveTheme, Disableable,
    button::{Button, ButtonVariants},
};
use std::{
    sync::mpsc,
    time::{Duration, Instant},
};
use verifact_core::{Core, ExecutionPlan, RunOptions, RunRecord, RunStatus};

#[derive(Clone)]
pub struct ExecutionUpdated {
    pub statuses: std::collections::BTreeMap<String, RunStatus>,
}

impl EventEmitter<ExecutionUpdated> for ExecutionPanel {}

enum Update {
    Planned(Result<ExecutionPlan, String>),
    Started(Result<RunRecord, String>),
    Cancelled(Result<RunRecord, String>),
    Refreshed(Result<(Vec<RunRecord>, String), String>),
}

pub struct ExecutionPanel {
    core: Core,
    project_id: Option<String>,
    selected: Option<String>,
    visible: Vec<String>,
    generation: u64,
    plan: Option<ExecutionPlan>,
    plan_page: usize,
    history: Vec<RunRecord>,
    error: Option<String>,
    busy: bool,
    refreshing: bool,
    last_poll: Instant,
    expanded: bool,
    output_open: bool,
    output: String,
    sender: mpsc::Sender<(u64, Update)>,
    receiver: mpsc::Receiver<(u64, Update)>,
}

impl ExecutionPanel {
    pub fn new(core: Core, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let (sender, receiver) = mpsc::channel();
        cx.spawn_in(window, async move |entity, cx| {
            loop {
                if entity.update_in(cx, |this, _, cx| this.tick(cx)).is_err() {
                    break;
                }
                Timer::after(Duration::from_millis(150)).await;
            }
        })
        .detach();
        Self {
            core,
            project_id: None,
            selected: None,
            visible: Vec::new(),
            generation: 0,
            plan: None,
            plan_page: 0,
            history: Vec::new(),
            error: None,
            busy: false,
            refreshing: false,
            last_poll: Instant::now(),
            expanded: false,
            output_open: false,
            output: String::new(),
            sender,
            receiver,
        }
    }

    pub fn set_context(
        &mut self,
        project_id: Option<String>,
        selected: Option<String>,
        visible: Vec<String>,
        cx: &mut Context<Self>,
    ) {
        if self.project_id != project_id {
            self.generation = self.generation.wrapping_add(1);
            self.project_id = project_id;
            self.history.clear();
            self.plan = None;
            self.error = None;
            self.output.clear();
            self.busy = false;
            self.refreshing = false;
            self.last_poll = Instant::now()
                .checked_sub(Duration::from_secs(2))
                .unwrap_or_else(Instant::now);
        }
        self.selected = selected;
        self.visible = visible;
        cx.notify();
    }

    fn tick(&mut self, cx: &mut Context<Self>) {
        while let Ok((generation, update)) = self.receiver.try_recv() {
            if generation != self.generation {
                continue;
            }
            match update {
                Update::Planned(result) => {
                    self.busy = false;
                    match result {
                        Ok(plan) => {
                            self.plan_page = 0;
                            self.plan = Some(plan);
                            self.error = None;
                            self.expanded = true;
                        }
                        Err(error) => self.error = Some(error),
                    }
                }
                Update::Started(result) | Update::Cancelled(result) => {
                    self.busy = false;
                    match result {
                        Ok(run) => {
                            self.history.retain(|existing| existing.id != run.id);
                            self.history.insert(0, run);
                            self.plan = None;
                            self.expanded = true;
                            self.error = None;
                        }
                        Err(error) => self.error = Some(error),
                    }
                    self.last_poll = Instant::now()
                        .checked_sub(Duration::from_secs(2))
                        .unwrap_or_else(Instant::now);
                }
                Update::Refreshed(result) => {
                    self.refreshing = false;
                    match result {
                        Ok((history, output)) => {
                            self.history = history;
                            self.output = output;
                        }
                        Err(error) => self.error = Some(error),
                    }
                }
            }
            self.publish_statuses(cx);
            cx.notify();
        }
        if !self.refreshing
            && self.last_poll.elapsed() >= Duration::from_millis(700)
            && let Some(project_id) = self.project_id.clone()
        {
            self.refreshing = true;
            self.last_poll = Instant::now();
            let core = self.core.clone();
            let sender = self.sender.clone();
            let generation = self.generation;
            std::thread::spawn(move || {
                let result = (|| -> anyhow::Result<_> {
                    let history = core.run_history(&project_id, 10)?;
                    let output = if let Some(run) = history.first() {
                        core.run_output(&run.id, 0, 65536)?.text
                    } else {
                        String::new()
                    };
                    Ok((history, output))
                })()
                .map_err(|error| format!("{error:#}"));
                let _ = sender.send((generation, Update::Refreshed(result)));
            });
        }
    }

    fn publish_statuses(&self, cx: &mut Context<Self>) {
        let mut statuses = std::collections::BTreeMap::new();
        for run in &self.history {
            for id in &run.plan.candidate_ids {
                statuses.entry(id.clone()).or_insert_with(|| {
                    if !run.status.is_final()
                        || matches!(
                            run.status,
                            RunStatus::TimedOut
                                | RunStatus::Cancelled
                                | RunStatus::Blocked
                                | RunStatus::InfrastructureError
                        )
                    {
                        return run.status;
                    }
                    let cases: Vec<_> = run
                        .results
                        .iter()
                        .filter(|case| case.candidate_id.as_ref() == Some(id))
                        .collect();
                    if cases.is_empty() {
                        return run.status;
                    }
                    if let Some(case) = cases
                        .iter()
                        .find(|case| !matches!(case.status, RunStatus::Passed | RunStatus::Skipped))
                    {
                        return case.status;
                    }
                    if cases.iter().any(|case| case.status == RunStatus::Passed) {
                        RunStatus::Passed
                    } else {
                        RunStatus::Skipped
                    }
                });
            }
        }
        cx.emit(ExecutionUpdated { statuses });
    }

    fn prepare(&mut self, selected: bool, cx: &mut Context<Self>) {
        let Some(project) = self.project_id.clone() else {
            return;
        };
        let ids = if selected {
            self.selected.iter().cloned().collect()
        } else {
            self.visible.clone()
        };
        if ids.is_empty() {
            self.error = Some("Choose a candidate file first.".into());
            cx.notify();
            return;
        }
        self.busy = true;
        self.error = None;
        self.plan = None;
        let core = self.core.clone();
        let sender = self.sender.clone();
        let generation = self.generation;
        std::thread::spawn(move || {
            let result = core
                .plan_run(&project, &ids, RunOptions::default())
                .map_err(|error| format!("{error:#}"));
            let _ = sender.send((generation, Update::Planned(result)));
        });
        cx.notify();
    }

    fn approve(&mut self, cx: &mut Context<Self>) {
        let Some(plan) = self.plan.take() else {
            return;
        };
        self.busy = true;
        let core = self.core.clone();
        let sender = self.sender.clone();
        let generation = self.generation;
        std::thread::spawn(move || {
            let result = core
                .start_run(plan, true)
                .map_err(|error| format!("{error:#}"));
            let _ = sender.send((generation, Update::Started(result)));
        });
        cx.notify();
    }

    fn cancel(&mut self, id: String, cx: &mut Context<Self>) {
        self.busy = true;
        let core = self.core.clone();
        let sender = self.sender.clone();
        let generation = self.generation;
        std::thread::spawn(move || {
            let result = core.cancel_run(&id).map_err(|error| format!("{error:#}"));
            let _ = sender.send((generation, Update::Cancelled(result)));
        });
        cx.notify();
    }
}

impl Render for ExecutionPanel {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let palette = Palette::for_dark(cx.theme().is_dark());
        let active = self
            .history
            .iter()
            .find(|run| !run.status.is_final())
            .map(|run| run.id.clone());
        div()
            .flex()
            .flex_col()
            .gap_2()
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(
                        Button::new("prepare-selected")
                            .primary()
                            .child(design::action_label("Run selected", &palette, true))
                            .disabled(self.busy || self.selected.is_none())
                            .on_click(cx.listener(|this, _, _, cx| this.prepare(true, cx))),
                    )
                    .child(
                        Button::new("prepare-visible")
                            .child(design::action_label("Run filtered files", &palette, false))
                            .disabled(self.busy || self.visible.is_empty())
                            .on_click(cx.listener(|this, _, _, cx| this.prepare(false, cx))),
                    )
                    .child(
                        Button::new("execution-history")
                            .ghost()
                            .child(design::action_label(
                                if self.expanded {
                                    "Hide execution"
                                } else {
                                    "Execution history"
                                },
                                &palette,
                                false,
                            ))
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.expanded = !this.expanded;
                                cx.notify();
                            })),
                    )
                    .when_some(active, |element, id| {
                        element.child(
                            Button::new("cancel-execution")
                                .child(design::action_label("Cancel run", &palette, false))
                                .disabled(self.busy)
                                .on_click(
                                    cx.listener(move |this, _, _, cx| this.cancel(id.clone(), cx)),
                                ),
                        )
                    }),
            )
            .when_some(self.error.clone(), |element, error| {
                element.child(div().text_color(palette.failure).child(error))
            })
            .when(self.expanded, |element| {
                element.child(self.render_details(&palette, cx))
            })
    }
}

impl ExecutionPanel {
    fn render_details(&self, palette: &Palette, cx: &mut Context<Self>) -> Stateful<Div> {
        let mut panel = div()
            .id("execution-details")
            .max_h(px(240.))
            .overflow_y_scroll()
            .p_3()
            .bg(palette.surface)
            .rounded_md()
            .flex()
            .flex_col()
            .gap_2();
        if let Some(plan) = &self.plan {
            panel=panel.child(div().font_weight(FontWeight::SEMIBOLD).child("Review execution"))
                .child(div().text_color(palette.warning).child("This runs project code and may change application data. Approve only for your intended test environment."))
                .children(plan.commands.iter().skip(self.plan_page * 12).take(12).map(|command|div().font_family(design::FONT_CODE).text_size(px(design::META_SIZE)).child(format!("{} {:?}\nIn {}",command.program.display(),command.args,command.cwd.display()))))
                .child(div().flex().gap_2().items_center()
                    .child(Button::new("previous-plan-page").ghost().child(design::action_label("Previous commands",palette,false)).disabled(self.plan_page==0).on_click(cx.listener(|this,_,_,cx|{this.plan_page=this.plan_page.saturating_sub(1);cx.notify();})))
                    .child(format!("Commands {}–{} of {}",self.plan_page*12+1,((self.plan_page+1)*12).min(plan.commands.len()),plan.commands.len()))
                    .child(Button::new("next-plan-page").ghost().child(design::action_label("Next commands",palette,false)).disabled((self.plan_page+1)*12>=plan.commands.len()).on_click(cx.listener(|this,_,_,cx|{this.plan_page+=1;cx.notify();}))))
                .child(format!("{} file commands · {} ms timeout · concurrency {}",plan.commands.len(),plan.options.timeout_ms,plan.options.max_concurrency))
                .child(Button::new("approve-run").primary().child(design::action_label(format!("Approve all {} commands and run",plan.commands.len()),palette,true)).disabled(self.busy).on_click(cx.listener(|this,_,_,cx|this.approve(cx))));
        }
        if self.history.is_empty() && self.plan.is_none() {
            panel = panel.child("No executions yet. Select files and review a run to begin.");
        }
        for run in self.history.iter().take(5) {
            let color = match run.status {
                verifact_core::RunStatus::Passed => palette.success,
                verifact_core::RunStatus::Failed
                | verifact_core::RunStatus::InfrastructureError => palette.failure,
                _ => palette.secondary,
            };
            panel = panel
                .child(div().text_color(color).child(format!(
                    "{} · {} · {} result records · {} ms",
                    run.queued_at,
                    run.status.as_str(),
                    run.results.len(),
                    run.duration_ms.unwrap_or(0)
                )))
                .when_some(run.message.clone(), |element, message| {
                    element.child(div().text_color(palette.secondary).child(message))
                });
        }
        if let Some(run) = self.history.first() {
            for result in run.results.iter().take(12) {
                panel = panel.child(div().text_size(px(design::META_SIZE)).child(format!(
                        "{} · {}{}",
                        result.status.as_str(),
                        result.name,
                        result
                            .message
                            .as_ref()
                            .map(|message| format!(" — {message}"))
                            .unwrap_or_default()
                    )));
            }
            panel = panel
                .child(
                    Button::new("toggle-run-output")
                        .ghost()
                        .child(design::action_label(
                            if self.output_open {
                                "Hide runner output"
                            } else {
                                "Runner output"
                            },
                            palette,
                            false,
                        ))
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.output_open = !this.output_open;
                            cx.notify();
                        })),
                )
                .when(self.output_open, |element| {
                    element.child(
                        div()
                            .font_family(design::FONT_CODE)
                            .text_size(px(design::META_SIZE))
                            .child(if self.output.is_empty() {
                                "No output captured yet.".to_owned()
                            } else {
                                self.output.clone()
                            }),
                    )
                });
        }
        panel
    }
}
