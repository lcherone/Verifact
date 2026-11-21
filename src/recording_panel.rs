//! Contextual native browser recorder. The hosted website occupies the empty
//! center; worker IPC and persistence never run on the GPUI thread.
use crate::design::{self, Palette};
use gpui::{prelude::*, *};
use gpui_component::{
    ActiveTheme, Disableable,
    button::Button,
    input::{Input, InputState},
};
use serde_json::{Value, json};
use std::{
    path::PathBuf,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    time::{Duration, Instant},
};
use verifact_core::{BrowserWorker, BrowserWorkerControl, Core, Project};

#[derive(Clone)]
pub struct RecordingLayout {
    pub inspector_width: f32,
}
impl EventEmitter<RecordingLayout> for RecordingPanel {}

#[derive(Default)]
struct Session {
    worker: Mutex<Option<BrowserWorker>>,
    control: Mutex<Option<BrowserWorkerControl>>,
    closing: AtomicBool,
}
impl Session {
    fn close(self: Arc<Self>) -> mpsc::Receiver<()> {
        self.closing.store(true, Ordering::SeqCst);
        let (sender, receiver) = mpsc::channel();
        std::thread::spawn(move || {
            if let Ok(control) = self.control.lock()
                && let Some(control) = control.as_ref()
            {
                control.shutdown();
            }
            if let Ok(mut worker) = self.worker.lock() {
                worker.take();
            }
            let _ = sender.send(());
        });
        receiver
    }
}
struct Update {
    generation: u64,
    method: String,
    result: Result<Value, String>,
    recording: Option<Value>,
    list: Option<Value>,
    events: Vec<Value>,
}
#[derive(Clone, Copy, PartialEq)]
enum Inspector {
    Capture,
    Library,
    Evidence,
    Source,
}
#[derive(Clone, Copy)]
enum Review {
    Replay,
    Verify,
    Export,
    Discard,
}

pub struct RecordingPanel {
    data_dir: PathBuf,
    port: u16,
    project: Option<Project>,
    generation: u64,
    session: Arc<Session>,
    sender: mpsc::Sender<Update>,
    receiver: mpsc::Receiver<Update>,
    busy: bool,
    polling: bool,
    last_poll: Instant,
    open: bool,
    section: Inspector,
    recording: Option<Value>,
    recordings: Vec<Value>,
    error: Option<String>,
    message: Option<String>,
    operation: Option<String>,
    review: Option<Review>,
    source: Option<Value>,
    evidence: Option<Value>,
    step_page: usize,
    library_page: usize,
    assertion_kind: &'static str,
    name: Entity<InputState>,
    note: Entity<InputState>,
    selector: Entity<InputState>,
    expected: Entity<InputState>,
    redact: Entity<InputState>,
    step_editor: Entity<InputState>,
    editing_step: Option<usize>,
}
impl RecordingPanel {
    pub fn new(core: Core, port: u16, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let (sender, receiver) = mpsc::channel();
        let name = cx.new(|cx| InputState::new(window, cx).placeholder("Journey name"));
        let note = cx.new(|cx| InputState::new(window, cx).placeholder("Note or checkpoint label"));
        let selector = cx.new(|cx| {
            InputState::new(window, cx).placeholder("CSS selector, e.g. [data-testid=total]")
        });
        let expected = cx.new(|cx| {
            InputState::new(window, cx).placeholder("Expected URL, text, value or count")
        });
        let redact = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("Extra screenshot redaction selectors, one per line")
                .multi_line(true)
                .rows(2)
        });
        let step_editor = cx.new(|cx| {
            InputState::new(window, cx)
                .multi_line(true)
                .rows(7)
                .placeholder("Structured step JSON")
        });
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
            data_dir: core.data_dir().join("browser-recordings"),
            port,
            project: None,
            generation: 0,
            session: Arc::default(),
            sender,
            receiver,
            busy: false,
            polling: false,
            last_poll: Instant::now(),
            open: false,
            section: Inspector::Capture,
            recording: None,
            recordings: vec![],
            error: None,
            message: None,
            operation: None,
            review: None,
            source: None,
            evidence: None,
            step_page: 0,
            library_page: 0,
            assertion_kind: "visible",
            name,
            note,
            selector,
            expected,
            redact,
            step_editor,
            editing_step: None,
        }
    }
    pub fn set_project(&mut self, project: Option<Project>, cx: &mut Context<Self>) {
        if self.project.as_ref().map(|project| &project.id)
            == project.as_ref().map(|project| &project.id)
        {
            return;
        }
        let _ = self.session.clone().close();
        self.session = Arc::default();
        self.project = project;
        self.generation = self.generation.wrapping_add(1);
        self.recording = None;
        self.recordings.clear();
        self.source = None;
        self.evidence = None;
        self.error = None;
        self.message = None;
        self.review = None;
        self.busy = false;
        self.polling = false;
        self.operation = None;
        self.editing_step = None;
        self.step_page = 0;
        self.library_page = 0;
        cx.notify();
    }
    /// Signal immediately, then wait for completion on a background thread before
    /// shutting down CEF. The receiver must never be waited on from the UI thread.
    pub fn shutdown(&mut self) -> mpsc::Receiver<()> {
        self.session.clone().close()
    }
    fn toggle(&mut self, cx: &mut Context<Self>) {
        self.open = !self.open;
        cx.emit(RecordingLayout {
            inspector_width: if self.open { 360. } else { 0. },
        });
        if self.open && self.project.is_some() && !self.busy {
            self.request("recording.list", json!({}), cx);
        }
        cx.notify();
    }
    fn state(&self) -> &str {
        self.recording
            .as_ref()
            .and_then(|value| value.get("state"))
            .and_then(Value::as_str)
            .unwrap_or("idle")
    }
    fn steps(&self) -> Vec<Value> {
        self.recording
            .as_ref()
            .and_then(|value| value.get("steps"))
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default()
    }
    fn recording_id(&self) -> Option<String> {
        self.recording
            .as_ref()
            .and_then(|value| value.get("id"))
            .and_then(Value::as_str)
            .map(str::to_owned)
    }
    fn request(&mut self, method: &str, mut params: Value, cx: &mut Context<Self>) {
        if self.busy || self.project.is_none() {
            return;
        }
        self.busy = true;
        self.error = None;
        if let Some(project) = &self.project {
            params["projectId"] = json!(project.id);
        }
        self.operation = Some(method.to_owned());
        self.launch(method, params, false);
        cx.notify();
    }
    fn launch(&self, method: &str, params: Value, poll: bool) {
        let session = self.session.clone();
        let sender = self.sender.clone();
        let generation = self.generation;
        let Some(project) = &self.project else {
            return;
        };
        let directory = self.data_dir.join(&project.id);
        let port = self.port;
        let method = method.to_owned();
        let project_id = project.id.clone();
        std::thread::spawn(move || {
            let mut recording = None;
            let mut list = None;
            let mut events = vec![];
            let result = (|| -> anyhow::Result<Value> {
                anyhow::ensure!(
                    !session.closing.load(Ordering::SeqCst),
                    "Recorder is shutting down"
                );
                let mut guard = session
                    .worker
                    .lock()
                    .map_err(|_| anyhow::anyhow!("Recorder worker unavailable"))?;
                if guard.is_none() {
                    let mut worker = BrowserWorker::spawn(&directory)?;
                    *session
                        .control
                        .lock()
                        .map_err(|_| anyhow::anyhow!("Recorder control unavailable"))? =
                        Some(worker.control());
                    anyhow::ensure!(
                        !session.closing.load(Ordering::SeqCst),
                        "Recorder is shutting down"
                    );
                    worker.request(
                        "attach",
                        json!({"endpoint":format!("http://127.0.0.1:{port}")}),
                    )?;
                    *guard = Some(worker);
                }
                let worker = guard
                    .as_mut()
                    .ok_or_else(|| anyhow::anyhow!("Recorder worker unavailable"))?;
                let value = worker.request(&method, params)?;
                recording = worker
                    .request("recording.status", json!({}))?
                    .get("recording")
                    .filter(|value| !value.is_null())
                    .cloned();
                if !poll {
                    list = Some(worker.request("recording.list", json!({"projectId":project_id}))?);
                }
                events = worker.notifications();
                Ok(value)
            })()
            .map_err(|error| format!("{error:#}"));
            let _ = sender.send(Update {
                generation,
                method,
                result,
                recording,
                list,
                events,
            });
        });
    }
    fn tick(&mut self, cx: &mut Context<Self>) {
        while let Ok(update) = self.receiver.try_recv() {
            if update.generation != self.generation {
                continue;
            }
            if update.method == "recording.status" {
                self.polling = false;
            } else {
                self.busy = false;
                self.operation = None;
            }
            match update.result {
                Err(error) => {
                    self.error = Some(error);
                    if !self.open {
                        self.open = true;
                        cx.emit(RecordingLayout {
                            inspector_width: 360.,
                        });
                    }
                }
                Ok(value) => {
                    if update.method != "export" {
                        let old = self.recording.as_ref().map(|recording| {
                            (
                                recording.get("id").cloned(),
                                recording.get("revision").cloned(),
                            )
                        });
                        let new = update.recording.as_ref().map(|recording| {
                            (
                                recording.get("id").cloned(),
                                recording.get("revision").cloned(),
                            )
                        });
                        if old != new {
                            self.source = None;
                            self.review = None;
                        }
                        self.recording = update.recording;
                    }
                    if let Some(list) = update.list {
                        self.recordings = list
                            .get("recordings")
                            .and_then(Value::as_array)
                            .cloned()
                            .unwrap_or_default();
                    }
                    match update.method.as_str() {
                        "recording.generate_test" => {
                            self.source = Some(value);
                            self.section = Inspector::Source;
                        }
                        "recording.replay" | "recording.verify_generated_test" => {
                            self.message = Some(format!(
                                "{} · {} assertions",
                                value
                                    .get("status")
                                    .and_then(Value::as_str)
                                    .unwrap_or("unknown"),
                                value.get("assertions").and_then(Value::as_u64).unwrap_or(0)
                            ));
                            if update.method == "recording.verify_generated_test"
                                && let Some(source) = self.source.as_mut()
                            {
                                source["status"] =
                                    json!(if value.get("status").and_then(Value::as_str)
                                        == Some("passed")
                                        && source.get("sourceHash") == value.get("sourceHash")
                                    {
                                        "verified"
                                    } else {
                                        "unverified"
                                    });
                            }
                            self.evidence = Some(value);
                        }
                        "export" => {
                            self.message = Some(format!(
                                "Saved source to {}",
                                value
                                    .get("path")
                                    .and_then(Value::as_str)
                                    .unwrap_or("managed exports")
                            ));
                        }
                        method if method.starts_with("browser.") => {
                            self.evidence = Some(value);
                            self.section = Inspector::Evidence;
                        }
                        _ => {}
                    }
                    for event in update.events {
                        if event.get("method").and_then(Value::as_str) == Some("logs") {
                            self.message = event
                                .pointer("/params/message")
                                .and_then(Value::as_str)
                                .map(str::to_owned);
                        }
                    }
                }
            }
            cx.notify();
        }
        if (self.open || matches!(self.state(), "recording" | "paused"))
            && !self.busy
            && !self.polling
            && self.project.is_some()
            && self.last_poll.elapsed() >= Duration::from_millis(800)
        {
            self.polling = true;
            self.last_poll = Instant::now();
            self.launch("recording.status", json!({}), true);
        }
    }
    fn start(&mut self, cx: &mut Context<Self>) {
        let name = self.name.read(cx).value().trim().to_owned();
        let selectors: Vec<_> = self
            .redact
            .read(cx)
            .value()
            .lines()
            .map(str::trim)
            .filter(|line| !line.is_empty())
            .map(str::to_owned)
            .collect();
        self.request("recording.start",json!({"name":if name.is_empty(){"Untitled journey"}else{&name},"redactSelectors":selectors}),cx);
    }
    fn add_text(&mut self, kind: &str, cx: &mut Context<Self>) {
        let text = self.note.read(cx).value().to_string();
        self.request(&format!("recording.{kind}"), json!({"text":text}), cx);
    }
    fn assertion(&mut self, cx: &mut Context<Self>) {
        let value = self.expected.read(cx).value().to_string();
        let expected = if self.assertion_kind == "count" {
            match value.parse::<u64>() {
                Ok(value) => json!(value),
                Err(_) => {
                    self.error = Some("Count must be a nonnegative integer.".into());
                    cx.notify();
                    return;
                }
            }
        } else {
            json!(value)
        };
        let selector = self.selector.read(cx).value().to_string();
        self.request("recording.assertion",json!({"kind":self.assertion_kind,"expected":expected,"target":{"candidates":[{"kind":"css","value":selector}]}}),cx);
    }
    fn review(&mut self, review: Review, cx: &mut Context<Self>) {
        self.review = Some(review);
        self.open = true;
        cx.emit(RecordingLayout {
            inspector_width: 360.,
        });
        cx.notify();
    }
    fn approve(&mut self, cx: &mut Context<Self>) {
        match self.review.take() {
            Some(Review::Replay) => {
                self.request("recording.replay", json!({"id":self.recording_id()}), cx)
            }
            Some(Review::Verify) => self.request(
                "recording.verify_generated_test",
                json!({"id":self.recording_id()}),
                cx,
            ),
            Some(Review::Discard) => {
                self.request("recording.discard", json!({"id":self.recording_id()}), cx)
            }
            Some(Review::Export) => self.export(cx),
            None => {}
        }
    }
    fn export(&mut self, cx: &mut Context<Self>) {
        let Some(source) = &self.source else {
            return;
        };
        let Some(text) = source.get("source").and_then(Value::as_str) else {
            return;
        };
        let Some(id) = source.get("recordingId").and_then(Value::as_str) else {
            return;
        };
        if !id
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || character == '-')
        {
            self.error = Some("Invalid recording export identity".into());
            return;
        }
        let Some(hash) = source
            .get("sourceHash")
            .and_then(Value::as_str)
            .filter(|hash| {
                hash.len() == 64 && hash.chars().all(|character| character.is_ascii_hexdigit())
            })
        else {
            self.error = Some("Invalid source fingerprint".into());
            return;
        };
        let directory = self
            .data_dir
            .join(
                self.project
                    .as_ref()
                    .map(|project| project.id.as_str())
                    .unwrap_or("unassigned"),
            )
            .join("exports");
        let file = directory.join(format!("{id}-{}.spec.mjs", &hash[..16]));
        let managed_root = self.data_dir.clone();
        let text = text.to_owned();
        let sender = self.sender.clone();
        let generation = self.generation;
        self.busy = true;
        std::thread::spawn(move || {
            let result = (|| -> anyhow::Result<Value> {
                std::fs::create_dir_all(&directory)?;
                anyhow::ensure!(
                    directory
                        .canonicalize()?
                        .starts_with(managed_root.canonicalize()?),
                    "Export path escapes managed recording storage"
                );
                use std::io::Write;
                let mut options = std::fs::OpenOptions::new();
                options.write(true).create_new(true);
                #[cfg(unix)]
                {
                    use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
                    options.mode(0o600);
                    std::fs::set_permissions(&directory, std::fs::Permissions::from_mode(0o700))?;
                }
                let mut file_handle = options.open(&file)?;
                file_handle.write_all(text.as_bytes())?;
                Ok(json!({"path":file}))
            })()
            .map_err(|error| format!("{error:#}"));
            let _ = sender.send(Update {
                generation,
                method: "export".into(),
                result,
                recording: None,
                list: None,
                events: vec![],
            });
        });
        cx.notify();
    }
    fn cancel(&mut self, cx: &mut Context<Self>) {
        let session = self.session.clone();
        std::thread::spawn(move || {
            if let Ok(control) = session.control.lock()
                && let Some(control) = control.as_ref()
            {
                let _ = control.cancel_replay();
            }
        });
        self.message = Some("Cancellation requested…".into());
        cx.notify();
    }
    fn button(
        &self,
        id: impl Into<ElementId>,
        label: &str,
        disabled: bool,
        palette: &Palette,
    ) -> Button {
        Button::new(id)
            .child(design::action_label(label.to_owned(), palette, false))
            .disabled(disabled)
    }
}

impl Render for RecordingPanel {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let palette = Palette::for_dark(cx.theme().is_dark());
        let state = self.state().to_owned();
        let active = matches!(state.as_str(), "recording" | "paused");
        let completed = matches!(state.as_str(), "completed" | "interrupted");
        let count = self.steps().len();
        div()
            .size_full()
            .flex()
            .flex_col()
            .text_color(palette.text)
            .font_family(design::FONT_UI)
            .text_size(px(design::BODY_SIZE))
            .child(
                div()
                    .h(px(44.))
                    .flex_shrink_0()
                    .flex()
                    .items_center()
                    .gap_2()
                    .px_3()
                    .bg(palette.surface)
                    .border_b_1()
                    .border_color(palette.border)
                    .child(
                        self.button(
                            "record-start",
                            "Record",
                            self.busy || active || self.project.is_none(),
                            &palette,
                        )
                        .on_click(cx.listener(|this, _, _, cx| this.start(cx))),
                    )
                    .child(
                        self.button(
                            "record-pause",
                            if state == "paused" { "Resume" } else { "Pause" },
                            self.busy || !active,
                            &palette,
                        )
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.request(
                                if this.state() == "paused" {
                                    "recording.resume"
                                } else {
                                    "recording.pause"
                                },
                                json!({}),
                                cx,
                            )
                        })),
                    )
                    .child(
                        self.button("record-stop", "Complete", self.busy || !active, &palette)
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.request("recording.stop", json!({}), cx)
                            })),
                    )
                    .child(
                        div()
                            .flex_1()
                            .text_color(if state == "recording" {
                                palette.failure
                            } else {
                                palette.secondary
                            })
                            .child(if self.busy {
                                match self.operation.as_deref() {
                                    Some("recording.start") => "Starting recording…",
                                    Some("recording.pause") => "Pausing…",
                                    Some("recording.resume") => "Resuming…",
                                    Some("recording.stop") => "Completing recording…",
                                    Some("recording.replay") => "Replaying in isolation…",
                                    Some("recording.verify_generated_test") => {
                                        "Verifying generated test…"
                                    }
                                    Some("recording.generate_test") => "Generating test…",
                                    _ => "Working…",
                                }
                                .to_owned()
                            } else {
                                format!("{state} · {count} steps")
                            }),
                    )
                    .when(
                        self.busy
                            && self.operation.as_ref().is_some_and(|method| {
                                matches!(
                                    method.as_str(),
                                    "recording.replay" | "recording.verify_generated_test"
                                )
                            }),
                        |toolbar| {
                            toolbar.child(
                                self.button("record-cancel", "Cancel", false, &palette)
                                    .on_click(cx.listener(|this, _, _, cx| this.cancel(cx))),
                            )
                        },
                    )
                    .child(
                        self.button(
                            "record-inspector",
                            if self.open {
                                "Hide recorder"
                            } else {
                                "Recorder"
                            },
                            false,
                            &palette,
                        )
                        .on_click(cx.listener(|this, _, _, cx| this.toggle(cx))),
                    ),
            )
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .flex()
                    .child(div().flex_1().min_w_0())
                    .when(self.open, |workspace| {
                        workspace.child(self.inspector(&palette, active, completed, cx))
                    }),
            )
    }
}
impl RecordingPanel {
    fn inspector(
        &self,
        palette: &Palette,
        active: bool,
        completed: bool,
        cx: &mut Context<Self>,
    ) -> Stateful<Div> {
        let mut panel = div()
            .id("recording-inspector")
            .w(px(360.))
            .flex_shrink_0()
            .h_full()
            .overflow_y_scroll()
            .p_3()
            .flex()
            .flex_col()
            .gap_3()
            .bg(palette.surface)
            .border_l_1()
            .border_color(palette.border);
        panel = panel.child(
            div().flex().flex_wrap().gap_1().children(
                [
                    (Inspector::Capture, "Capture"),
                    (Inspector::Library, "Journeys"),
                    (Inspector::Evidence, "Evidence"),
                    (Inspector::Source, "Source"),
                ]
                .into_iter()
                .enumerate()
                .map(|(index, (section, label))| {
                    self.button(("record-tab", index), label, false, palette)
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.section = section;
                            if section == Inspector::Library {
                                this.request("recording.list", json!({}), cx);
                            }
                            cx.notify();
                        }))
                }),
            ),
        );
        if let Some(error) = &self.error {
            panel = panel.child(div().text_color(palette.failure).child(error.clone()));
        }
        if let Some(message) = &self.message {
            panel = panel.child(div().text_color(palette.secondary).child(message.clone()));
        }
        if self.project.is_none() {
            return panel.child("Choose a project to record its browser journey.");
        }
        if let Some(review) = self.review {
            let text = match review {
                Review::Replay => {
                    "Replay opens a fresh isolated browser and performs every recorded action. It may change application data. Saved authentication is not copied. Explicit assertions are required for verification."
                }
                Review::Verify => {
                    "Run the exact generated Playwright test in a fresh isolated browser. This executes recorded actions and may change application data. Verification applies to this source revision only."
                }
                Review::Export => {
                    "Save this generated source revision to the app-managed exports folder below. Existing files are never overwritten."
                }
                Review::Discard => {
                    "Permanently discard this saved recording. This removes its recording data; existing exported files are retained."
                }
            };
            panel = panel.child(
                div()
                    .p_2()
                    .bg(palette.raised)
                    .flex()
                    .flex_col()
                    .gap_2()
                    .child(text)
                    .when(matches!(review, Review::Export), |element| {
                        element.child(
                            self.data_dir
                                .join(
                                    self.project
                                        .as_ref()
                                        .map(|project| project.id.as_str())
                                        .unwrap_or(""),
                                )
                                .join("exports")
                                .display()
                                .to_string(),
                        )
                    })
                    .child(
                        self.button("record-approve", "Approve", self.busy, palette)
                            .on_click(cx.listener(|this, _, _, cx| this.approve(cx))),
                    )
                    .child(
                        self.button("record-dismiss", "Cancel", self.busy, palette)
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.review = None;
                                cx.notify();
                            })),
                    ),
            );
        }
        match self.section {
            Inspector::Capture => {
                panel=panel.child("Journey name").child(Input::new(&self.name))
                    .child(div().text_color(palette.secondary).child("Input values are captured as secret references. Add selectors to mask other sensitive content in screenshots."))
                    .child(Input::new(&self.redact));
                if let Some(recording) = &self.recording {
                    let verified = recording
                        .pointer("/verification/status")
                        .and_then(Value::as_str)
                        .unwrap_or("unverified");
                    panel = panel.child(format!(
                        "{} · {} · revision {}",
                        recording
                            .get("name")
                            .and_then(Value::as_str)
                            .unwrap_or("Journey"),
                        verified,
                        recording
                            .get("revision")
                            .and_then(Value::as_u64)
                            .unwrap_or(0)
                    ));
                }
                panel = panel
                    .child(Input::new(&self.note))
                    .child(
                        div()
                            .flex()
                            .gap_2()
                            .child(
                                self.button(
                                    "record-note",
                                    "Add note",
                                    self.busy || !active,
                                    palette,
                                )
                                .on_click(cx.listener(|this, _, _, cx| this.add_text("note", cx))),
                            )
                            .child(
                                self.button(
                                    "record-checkpoint",
                                    "Checkpoint",
                                    self.busy || !active,
                                    palette,
                                )
                                .on_click(
                                    cx.listener(|this, _, _, cx| this.add_text("checkpoint", cx)),
                                ),
                            ),
                    )
                    .child("Assertion kind")
                    .child(
                        div().flex().flex_wrap().gap_1().children(
                            [
                                "visible", "hidden", "text", "contains", "value", "url", "count",
                            ]
                            .into_iter()
                            .enumerate()
                            .map(|(index, kind)| {
                                self.button(
                                    ("assertion-kind", index),
                                    if self.assertion_kind == kind {
                                        format!("• {kind}")
                                    } else {
                                        kind.to_owned()
                                    }
                                    .as_str(),
                                    self.busy,
                                    palette,
                                )
                                .on_click(cx.listener(
                                    move |this, _, _, cx| {
                                        this.assertion_kind = kind;
                                        cx.notify();
                                    },
                                ))
                            }),
                        ),
                    )
                    .when(self.assertion_kind != "url", |element| {
                        element.child(Input::new(&self.selector))
                    })
                    .when(
                        !matches!(self.assertion_kind, "visible" | "hidden"),
                        |element| element.child(Input::new(&self.expected)),
                    )
                    .child(
                        self.button(
                            "record-assertion",
                            "Add and check assertion",
                            self.busy || !active,
                            palette,
                        )
                        .on_click(cx.listener(|this, _, _, cx| this.assertion(cx))),
                    )
                    .child(
                        div()
                            .flex()
                            .flex_wrap()
                            .gap_2()
                            .child(
                                self.button(
                                    "record-replay",
                                    "Review replay",
                                    self.busy || !completed,
                                    palette,
                                )
                                .on_click(
                                    cx.listener(|this, _, _, cx| this.review(Review::Replay, cx)),
                                ),
                            )
                            .child(
                                self.button(
                                    "record-generate",
                                    "Generate test",
                                    self.busy || !completed,
                                    palette,
                                )
                                .on_click(cx.listener(
                                    |this, _, _, cx| {
                                        this.request(
                                            "recording.generate_test",
                                            json!({"id":this.recording_id()}),
                                            cx,
                                        )
                                    },
                                )),
                            ),
                    )
                    .when(completed, |element| {
                        element.child(
                            div()
                                .flex()
                                .gap_2()
                                .child(
                                    self.button("record-rename", "Rename", self.busy, palette)
                                        .on_click(cx.listener(|this, _, _, cx| {
                                            let name = this.name.read(cx).value().to_string();
                                            this.request(
                                                "recording.edit",
                                                json!({"id":this.recording_id(),"name":name}),
                                                cx,
                                            );
                                        })),
                                )
                                .child(
                                    self.button("record-discard", "Discard…", self.busy, palette)
                                        .on_click(cx.listener(|this, _, _, cx| {
                                            this.review(Review::Discard, cx)
                                        })),
                                ),
                        )
                    });
                let steps = self.steps();
                panel = panel.child(format!("Captured steps · {}", steps.len()));
                for (index, step) in steps.iter().enumerate().skip(self.step_page * 20).take(20) {
                    let summary = format!(
                        "{}. {}{}",
                        index + 1,
                        step.get("type").and_then(Value::as_str).unwrap_or("step"),
                        step.get("text")
                            .or_else(|| step.get("url"))
                            .or_else(|| step.get("kind"))
                            .and_then(Value::as_str)
                            .map(|value| format!(" · {value}"))
                            .unwrap_or_default()
                    );
                    panel = panel.child(
                        div()
                            .flex()
                            .flex_col()
                            .gap_1()
                            .p_2()
                            .bg(palette.raised)
                            .child(summary)
                            .when(completed, |element| {
                                element.child(
                                    self.button(
                                        ("edit-step", index),
                                        "Edit step…",
                                        self.busy,
                                        palette,
                                    )
                                    .on_click(cx.listener(
                                        move |this, _, window, cx| {
                                            if let Some(step) = this.steps().get(index) {
                                                let text = serde_json::to_string_pretty(step)
                                                    .unwrap_or_default();
                                                this.step_editor.update(cx, |editor, cx| {
                                                    editor.set_value(text, window, cx)
                                                });
                                                this.editing_step = Some(index);
                                                cx.notify();
                                            }
                                        },
                                    )),
                                )
                            })
                            .when(completed, |element| {
                                element.child(
                                    self.button(
                                        ("remove-step", index),
                                        "Remove step",
                                        self.busy,
                                        palette,
                                    )
                                    .on_click(cx.listener(
                                        move |this, _, _, cx| {
                                            let mut steps = this.steps();
                                            if index < steps.len() {
                                                steps.remove(index);
                                                this.request(
                                                    "recording.edit",
                                                    json!({"id":this.recording_id(),"steps":steps}),
                                                    cx,
                                                );
                                            }
                                        },
                                    )),
                                )
                            }),
                    );
                }
                if let Some(index) = self.editing_step {
                    panel = panel
                        .child(format!("Edit structured step {}", index + 1))
                        .child(Input::new(&self.step_editor))
                        .child(
                            self.button(
                                "save-step-edit",
                                "Save step",
                                self.busy || !completed,
                                palette,
                            )
                            .on_click(cx.listener(|this, _, _, cx| {
                                let Some(index) = this.editing_step else {
                                    return;
                                };
                                let text = this.step_editor.read(cx).value().to_string();
                                match serde_json::from_str::<Value>(&text) {
                                    Ok(step) => {
                                        let mut steps = this.steps();
                                        if index < steps.len() {
                                            steps[index] = step;
                                            this.editing_step = None;
                                            this.request(
                                                "recording.edit",
                                                json!({"id":this.recording_id(),"steps":steps}),
                                                cx,
                                            );
                                        }
                                    }
                                    Err(error) => {
                                        this.error = Some(format!("Invalid step JSON: {error}"));
                                        cx.notify();
                                    }
                                }
                            })),
                        )
                        .child(
                            self.button("dismiss-step-edit", "Cancel edit", self.busy, palette)
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.editing_step = None;
                                    cx.notify();
                                })),
                        );
                }
                panel = panel.child(
                    div()
                        .flex()
                        .gap_2()
                        .child(
                            self.button("step-previous", "Previous", self.step_page == 0, palette)
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.step_page = this.step_page.saturating_sub(1);
                                    cx.notify();
                                })),
                        )
                        .child(
                            self.button(
                                "step-next",
                                "Next",
                                (self.step_page + 1) * 20 >= steps.len(),
                                palette,
                            )
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.step_page += 1;
                                cx.notify();
                            })),
                        ),
                );
            }
            Inspector::Library => {
                if self.recordings.is_empty() {
                    panel = panel.child("No saved journeys for this project yet.");
                }
                for (index, recording) in self
                    .recordings
                    .iter()
                    .enumerate()
                    .skip(self.library_page * 20)
                    .take(20)
                {
                    let id = recording
                        .get("id")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_owned();
                    let label = format!(
                        "{} · {}",
                        recording
                            .get("name")
                            .and_then(Value::as_str)
                            .unwrap_or("Journey"),
                        recording
                            .get("state")
                            .and_then(Value::as_str)
                            .unwrap_or("unknown")
                    );
                    panel = panel.child(
                        self.button(
                            ("load-journey", index),
                            &label,
                            self.busy || active,
                            palette,
                        )
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.section = Inspector::Capture;
                            this.step_page = 0;
                            this.request("recording.load", json!({"id":id}), cx);
                        })),
                    );
                }
                panel = panel.child(
                    div()
                        .flex()
                        .gap_2()
                        .child(
                            self.button(
                                "journey-previous",
                                "Previous",
                                self.library_page == 0,
                                palette,
                            )
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.library_page = this.library_page.saturating_sub(1);
                                cx.notify();
                            })),
                        )
                        .child(
                            self.button(
                                "journey-next",
                                "Next",
                                (self.library_page + 1) * 20 >= self.recordings.len(),
                                palette,
                            )
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.library_page += 1;
                                cx.notify();
                            })),
                        ),
                );
            }
            Inspector::Evidence => {
                panel=panel.child("Captured evidence masks input fields and configured sensitive selectors. Console bodies and network bodies are omitted.")
                    .child(div().flex().flex_wrap().gap_2().children([("browser.screenshot","Screenshot"),("browser.snapshot","DOM snapshot"),("browser.console","Console metadata"),("browser.network","Network metadata"),("browser.trace.start","Start trace"),("browser.trace.stop","Stop trace")].into_iter().enumerate().map(|(index,(method,label))|self.button(("browser-evidence",index),label,self.busy,palette).on_click(cx.listener(move|this,_,_,cx|this.request(method,json!({}),cx))))));
                if let Some(evidence) = &self.evidence {
                    if serde_json::to_string(evidence)
                        .is_ok_and(|text| text.chars().count() > 24000)
                    {
                        panel = panel.child("Evidence preview is limited to 24,000 characters.");
                    }
                    panel = panel.child(
                        div()
                            .font_family(design::FONT_CODE)
                            .text_size(px(design::META_SIZE))
                            .child(
                                serde_json::to_string_pretty(evidence)
                                    .unwrap_or_default()
                                    .chars()
                                    .take(24000)
                                    .collect::<String>(),
                            ),
                    );
                }
            }
            Inspector::Source => {
                if let Some(source) = &self.source {
                    if source
                        .get("source")
                        .and_then(Value::as_str)
                        .is_some_and(|text| text.chars().count() > 64000)
                    {
                        panel=panel.child("Preview limited to 64,000 characters. Save source exports the complete revision.");
                    }
                    panel=panel.child(format!("Generated source · {}",source.get("status").and_then(Value::as_str).unwrap_or("unverified")))
                        .child(div().text_color(palette.secondary).child("Generated source is unverified until this exact revision passes its own Playwright execution with explicit assertions."))
                        .child(div().flex().flex_wrap().gap_2()
                            .child(self.button("source-verify","Review verification",self.busy,palette).on_click(cx.listener(|this,_,_,cx|this.review(Review::Verify,cx))))
                            .child(self.button("source-export","Save source…",self.busy,palette).on_click(cx.listener(|this,_,_,cx|this.review(Review::Export,cx)))))
                        .child(div().font_family(design::FONT_CODE).text_size(px(design::META_SIZE)).child(source.get("source").and_then(Value::as_str).unwrap_or_default().chars().take(64000).collect::<String>()));
                } else {
                    panel = panel.child("Complete a journey, then generate its Playwright test.");
                }
            }
        }
        panel
    }
}
