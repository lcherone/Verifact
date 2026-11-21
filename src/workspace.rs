//! Desktop presentation for the shared testing core. Scans and database reads
//! run on workers; browser rendering/input remain on the native main thread.
use crate::{
    browser_host::{BrowserHost, BrowserRuntime},
    catalogue::{CandidateSelected, CandidateTable},
    design::{self, Palette, ThemePreference},
};
use anyhow::{Context as _, Result};
use gpui::{prelude::*, *};
use gpui_component::{
    Disableable,
    button::{Button, ButtonVariants},
    input::{Input, InputEvent, InputState},
};
use std::{
    cell::RefCell,
    path::PathBuf,
    rc::Rc,
    sync::mpsc::{self, Receiver, Sender},
    time::Duration,
};
use verifact_core::{Core, JobRecord, Project, ScanReport, TestCandidate};

actions!(verifact_workspace, [AddProject, FocusSearch, FocusAddress]);

pub fn init(cx: &mut App) {
    cx.bind_keys([
        KeyBinding::new("cmd-o", AddProject, Some("Verifact")),
        KeyBinding::new("cmd-f", FocusSearch, Some("Verifact")),
        KeyBinding::new("cmd-l", FocusAddress, Some("Verifact")),
    ]);
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum WorkspaceMode {
    Projects,
    Browser,
}

struct LoadedProject {
    id: String,
    report: Option<ScanReport>,
    jobs: Vec<JobRecord>,
    base_url: Option<String>,
}

enum WorkerEvent {
    Registered(Project),
    ImportFailed(String),
    Error(String),
    Loaded {
        generation: u64,
        result: Result<LoadedProject, String>,
    },
    Scanned {
        project: Project,
        result: Result<ScanReport, String>,
    },
}

pub struct Workspace {
    core: Core,
    focus: FocusHandle,
    projects: Vec<Project>,
    metadata_generation: u64,
    selected: Option<Project>,
    report: Option<ScanReport>,
    jobs: Vec<JobRecord>,
    mode: WorkspaceMode,
    theme: ThemePreference,
    search: Entity<InputState>,
    catalogue: Entity<CandidateTable>,
    catalogue_dirty: bool,
    address: Entity<InputState>,
    framework_filter: Option<String>,
    selected_candidate: Option<String>,
    details_open: bool,
    project_base_url: Option<String>,
    busy: bool,
    activity: Option<String>,
    error: Option<String>,
    browser: Rc<RefCell<Option<BrowserHost>>>,
    close_requested: Rc<std::cell::Cell<bool>>,
    sender: Sender<WorkerEvent>,
    receiver: Receiver<WorkerEvent>,
    browser_only: bool,
    browser_port: u16,
    runtime: Rc<RefCell<Option<BrowserRuntime>>>,
    settings_sender: Sender<(String, String)>,
    pending_settings: std::sync::Arc<std::sync::atomic::AtomicUsize>,
}

impl Workspace {
    pub fn new(
        core: Core,
        browser_only: bool,
        browser_port: u16,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Result<Self> {
        let focus = cx.focus_handle();
        window.focus(&focus);
        let store = core.store()?;
        let projects = store.projects()?;
        let theme =
            ThemePreference::parse(&store.get_setting("theme")?.unwrap_or_else(|| "dark".into()));
        design::apply(theme, Some(window), cx);
        let selected_id = store.get_setting("selected_project")?;
        let selected =
            selected_id.and_then(|id| projects.iter().find(|project| project.id == id).cloned());
        let catalogue = cx.new(|cx| CandidateTable::new(window, cx));
        cx.subscribe(&catalogue, |this, _, event: &CandidateSelected, cx| {
            this.selected_candidate = Some(event.id.clone());
            cx.notify();
        })
        .detach();
        let search =
            cx.new(|cx| InputState::new(window, cx).placeholder("Search candidate test files…"));
        let url = std::env::var("VERIFACT_URL").unwrap_or_else(|_| "http://127.0.0.1:4179".into());
        let address = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("http://localhost:3000")
                .default_value(url.clone())
        });
        cx.subscribe(&search, |this, _, _: &InputEvent, cx| {
            this.catalogue_dirty = true;
            cx.notify();
        })
        .detach();
        cx.subscribe_in(&address, window, |this, _, event, _, cx| {
            if matches!(event, InputEvent::PressEnter { .. }) {
                this.navigate(cx);
            }
        })
        .detach();
        cx.observe_window_appearance(window, |this, window, cx| {
            design::apply(this.theme, Some(window), cx);
            cx.notify();
        })
        .detach();
        let (sender, receiver) = mpsc::channel();
        let (settings_sender, settings_receiver) = mpsc::channel::<(String, String)>();
        let pending_settings = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let pending = pending_settings.clone();
        let settings_core = core.clone();
        let settings_events = sender.clone();
        // Persist preferences in event order without holding the rendering thread
        // on SQLite locks or letting rapid theme/selection changes race.
        std::thread::spawn(move || {
            while let Ok((key, value)) = settings_receiver.recv() {
                if let Err(error) = settings_core
                    .store()
                    .and_then(|store| store.set_setting(&key, &value))
                {
                    let _ = settings_events.send(WorkerEvent::Error(format!(
                        "Could not save preference: {error:#}"
                    )));
                }
                pending.fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
            }
        });
        let browser: Rc<RefCell<Option<BrowserHost>>> = Rc::new(RefCell::new(None));
        let close_requested = Rc::new(std::cell::Cell::new(false));
        let closing = browser.clone();
        let requested = close_requested.clone();
        window.on_window_should_close(cx, move |_, _| {
            if let Some(browser) = closing.borrow_mut().as_mut() {
                browser.can_close_window()
            } else {
                requested.set(true);
                false
            }
        });
        let mut workspace = Self {
            core,
            focus,
            projects,
            metadata_generation: 0,
            selected,
            report: None,
            jobs: Vec::new(),
            mode: WorkspaceMode::Projects,
            theme,
            search,
            catalogue,
            catalogue_dirty: true,
            address,
            framework_filter: None,
            selected_candidate: None,
            details_open: false,
            project_base_url: None,
            busy: false,
            activity: None,
            error: None,
            browser,
            close_requested,
            sender,
            receiver,
            browser_only,
            browser_port,
            runtime: Rc::new(RefCell::new(None)),
            settings_sender,
            pending_settings,
        };
        if browser_only {
            workspace.show_browser(window, cx);
        } else if let Some(project) = workspace.selected.clone() {
            workspace.load_project(project, cx);
        }
        let startup_path = std::env::var_os("VERIFACT_PROJECT").map(PathBuf::from);
        if !browser_only && let Some(path) = startup_path {
            workspace.start_scan(Some(path), cx);
        }
        let runtime = workspace.runtime.clone();
        let lifecycle = workspace.browser.clone();
        let pending_preferences = workspace.pending_settings.clone();
        cx.spawn_in(window, async move |entity, cx| {
            loop {
                if let Some(runtime) = runtime.borrow().as_ref() {
                    runtime.pump();
                }
                let mut should_quit = entity
                    .update_in(cx, |workspace, window, cx| workspace.tick(window, cx))
                    .unwrap_or(true);
                if should_quit {
                    let mut browser = lifecycle.borrow_mut();
                    if let Some(host) = browser.as_mut() {
                        if host.is_closed() {
                            host.release_closed_browser();
                            browser.take();
                        } else {
                            host.close();
                            should_quit = false;
                        }
                    }
                }
                if should_quit && pending_preferences.load(std::sync::atomic::Ordering::SeqCst) == 0
                {
                    if let Some(runtime) = runtime.borrow_mut().take() {
                        runtime.shutdown();
                    }
                    crate::mac_application::allow_termination();
                    let _ = AsyncApp::update(cx, |cx| cx.quit());
                    break;
                }
                Timer::after(Duration::from_millis(8)).await;
            }
        })
        .detach();
        Ok(workspace)
    }

    fn palette(&self, window: &Window) -> Palette {
        Palette::for_dark(self.theme.resolved_dark(window.appearance()))
    }

    fn tick(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        if self.mode == WorkspaceMode::Browser {
            let current_url = self
                .browser
                .borrow()
                .as_ref()
                .filter(|host| !host.is_closed())
                .and_then(BrowserHost::current_url);
            if let Some(url) = current_url
                && !url.is_empty()
                && !self.address.read(cx).focus_handle(cx).is_focused(window)
                && self.address.read(cx).value().as_str() != url
            {
                self.address
                    .update(cx, |address, cx| address.set_value(url, window, cx));
                cx.notify();
            }
        }
        while let Ok(event) = self.receiver.try_recv() {
            match event {
                WorkerEvent::Error(error) => self.error = Some(error),
                WorkerEvent::ImportFailed(error) => {
                    self.busy = false;
                    self.activity = None;
                    self.error = Some(error);
                }
                WorkerEvent::Registered(project) => {
                    if self
                        .selected
                        .as_ref()
                        .is_none_or(|selected| selected.id != project.id)
                    {
                        self.report = None;
                        self.catalogue_dirty = true;
                        self.jobs.clear();
                        self.project_base_url = None;
                    }
                    if let Some(browser) = self.browser.borrow().as_ref() {
                        browser.set_visible(false);
                    }
                    if !self
                        .projects
                        .iter()
                        .any(|existing| existing.id == project.id)
                    {
                        self.projects.push(project.clone());
                        self.projects
                            .sort_by_key(|project| project.name.to_lowercase());
                    }
                    self.persist_setting("selected_project", &project.id);
                    self.selected = Some(project);
                    self.mode = WorkspaceMode::Projects;
                }
                WorkerEvent::Loaded { generation, result }
                    if generation == self.metadata_generation =>
                {
                    match result {
                        Ok(loaded)
                            if self
                                .selected
                                .as_ref()
                                .is_some_and(|project| project.id == loaded.id) =>
                        {
                            self.report = loaded.report;
                            self.catalogue_dirty = true;
                            self.jobs = loaded.jobs;
                            self.project_base_url = loaded.base_url;
                        }
                        Err(error) => self.error = Some(error),
                        _ => {}
                    }
                }
                WorkerEvent::Loaded { .. } => {}
                WorkerEvent::Scanned { project, result } => {
                    self.busy = false;
                    self.activity = None;
                    match result {
                        Ok(report) => {
                            if self
                                .selected
                                .as_ref()
                                .is_some_and(|selected| selected.id == project.id)
                            {
                                self.report = Some(report);
                                self.catalogue_dirty = true;
                            }
                            self.reload_metadata(project.id);
                        }
                        Err(error) => {
                            self.error = Some(error);
                            self.reload_metadata(project.id);
                        }
                    }
                }
            }
            cx.notify();
        }
        if self.catalogue_dirty {
            let candidates = self.filtered_candidates(cx);
            if self
                .selected_candidate
                .as_ref()
                .is_some_and(|id| !candidates.iter().any(|candidate| &candidate.id == id))
            {
                self.selected_candidate = None;
            }
            let report = self.report.clone();
            self.catalogue.update(cx, |table, cx| {
                table.set_entries(report.as_ref(), candidates, window, cx)
            });
            self.catalogue_dirty = false;
        }
        if self.close_requested.get() || crate::mac_application::quit_requested() {
            let mut browser = self.browser.borrow_mut();
            if let Some(browser) = browser.as_mut() {
                browser.close();
            } else {
                return self
                    .pending_settings
                    .load(std::sync::atomic::Ordering::SeqCst)
                    == 0;
            }
        }
        let mut browser = self.browser.borrow_mut();
        if let Some(host) = browser.as_mut()
            && host.is_closed()
        {
            host.release_closed_browser();
            browser.take();
            return true;
        }
        false
    }

    fn pick_project(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.busy {
            return;
        }
        let picker = cx.prompt_for_paths(PathPromptOptions {
            files: false,
            directories: true,
            multiple: false,
            prompt: Some("Add project".into()),
        });
        cx.spawn_in(window, async move |entity, cx| match picker.await {
            Ok(Ok(Some(paths))) => {
                if let Some(path) = paths.into_iter().next() {
                    let _ = entity.update(cx, |this, cx| this.start_scan(Some(path), cx));
                }
            }
            Ok(Ok(None)) => {}
            Ok(Err(error)) => {
                let _ = entity.update(cx, |this, cx| {
                    this.error = Some(format!("Cannot select project: {error}"));
                    cx.notify();
                });
            }
            Err(error) => {
                let _ = entity.update(cx, |this, cx| {
                    this.error = Some(format!("Directory picker closed unexpectedly: {error}"));
                    cx.notify();
                });
            }
        })
        .detach();
    }

    fn start_scan(&mut self, path: Option<PathBuf>, cx: &mut Context<Self>) {
        if self.busy {
            return;
        }
        if path.is_none() && self.selected.is_none() {
            return;
        }
        self.busy = true;
        self.error = None;
        self.activity = Some("Inspecting project files. No project code is being executed.".into());
        self.framework_filter = None;
        self.selected_candidate = None;
        let core = self.core.clone();
        let selected = self.selected.clone();
        let sender = self.sender.clone();
        std::thread::spawn(move || {
            let project = match path {
                Some(path) => core.add_project(path),
                None => selected.clone().context("No project is selected"),
            };
            match project {
                Ok(project) => {
                    let _ = sender.send(WorkerEvent::Registered(project.clone()));
                    let result = core.scan(&project).map_err(|error| format!("{error:#}"));
                    let _ = sender.send(WorkerEvent::Scanned { project, result });
                }
                Err(error) => {
                    let _ = sender.send(WorkerEvent::ImportFailed(format!("{error:#}")));
                }
            }
        });
        cx.notify();
    }

    fn reload_metadata(&mut self, id: String) {
        self.metadata_generation = self.metadata_generation.wrapping_add(1);
        let generation = self.metadata_generation;
        let sender = self.sender.clone();
        let core = self.core.clone();
        std::thread::spawn(move || {
            let loaded = (|| -> Result<LoadedProject> {
                let store = core.store()?;
                Ok(LoadedProject {
                    base_url: core.project_base_url(&id)?,
                    report: store.latest_scan(&id)?,
                    jobs: store.jobs(&id, 20)?,
                    id,
                })
            })()
            .map_err(|error| format!("{error:#}"));
            let _ = sender.send(WorkerEvent::Loaded {
                generation,
                result: loaded,
            });
        });
    }

    fn load_project(&mut self, project: Project, cx: &mut Context<Self>) {
        if self.busy {
            return;
        }
        self.selected = Some(project.clone());
        self.project_base_url = None;
        self.report = None;
        self.catalogue_dirty = true;
        self.jobs.clear();
        self.framework_filter = None;
        self.selected_candidate = None;
        self.mode = WorkspaceMode::Projects;
        if let Some(browser) = self.browser.borrow().as_ref() {
            browser.set_visible(false);
        }
        self.persist_setting("selected_project", &project.id);
        self.reload_metadata(project.id);
        cx.notify();
    }

    fn open_project_site(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(url) = self.project_base_url.clone() else {
            return;
        };
        let already_open = self.browser.borrow().is_some();
        self.address
            .update(cx, |address, cx| address.set_value(url, window, cx));
        self.show_browser(window, cx);
        // A new host loads the address during creation. Retarget only an
        // existing host, so opening a site does not send two navigations.
        if already_open && self.mode == WorkspaceMode::Browser {
            self.navigate(cx);
        }
    }

    fn show_browser(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.error = None;
        if self.runtime.borrow().is_none() {
            match crate::browser_host::initialize_runtime(self.browser_port, self.core.data_dir()) {
                Ok(runtime) => *self.runtime.borrow_mut() = Some(runtime),
                Err(error) => {
                    self.error = Some(format!("Browser could not start: {error:#}"));
                    cx.notify();
                    return;
                }
            }
        }
        let needs_browser = self.browser.borrow().is_none();
        let result = if needs_browser {
            BrowserHost::new(window, &self.address.read(cx).value()).map(|mut browser| {
                browser.set_workspace_layout(
                    if self.browser_only {
                        0.
                    } else {
                        design::SIDEBAR_WIDTH
                    },
                    design::HEADER_HEIGHT,
                );
                *self.browser.borrow_mut() = Some(browser);
            })
        } else {
            Ok(())
        };
        match result {
            Ok(()) => {
                self.mode = WorkspaceMode::Browser;
                if let Some(browser) = self.browser.borrow().as_ref() {
                    browser.set_visible(true);
                }
            }
            Err(error) => self.error = Some(format!("Browser could not open: {error:#}")),
        }
        cx.notify();
    }

    fn navigate(&mut self, cx: &mut Context<Self>) {
        let value = self.address.read(cx).value().to_string();
        match url::Url::parse(&value) {
            Ok(url) if matches!(url.scheme(), "http" | "https") => {
                if let Some(browser) = self.browser.borrow().as_ref() {
                    browser.navigate(url.as_str());
                }
                self.error = None;
            }
            _ => self.error = Some("Enter a complete http:// or https:// address.".into()),
        }
        cx.notify();
    }

    fn set_theme(
        &mut self,
        preference: ThemePreference,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.theme = preference;
        design::apply(preference, Some(window), cx);
        self.persist_setting("theme", preference.as_str());
        cx.notify();
    }

    fn persist_setting(&mut self, key: &str, value: &str) {
        self.pending_settings
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        if self
            .settings_sender
            .send((key.to_owned(), value.to_owned()))
            .is_err()
        {
            self.pending_settings
                .fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
            self.error = Some("Preference worker stopped. Restart Verifact to retry.".into());
        }
    }

    fn filtered_candidates(&self, cx: &App) -> Vec<TestCandidate> {
        let query = self.search.read(cx).value().to_lowercase();
        self.report
            .as_ref()
            .map(|report| {
                report
                    .candidates
                    .iter()
                    .filter(|candidate| {
                        candidate.display_name.to_lowercase().contains(&query)
                            && self
                                .framework_filter
                                .as_ref()
                                .is_none_or(|id| &candidate.framework_id == id)
                    })
                    .cloned()
                    .collect()
            })
            .unwrap_or_default()
    }

    fn render_sidebar(&self, palette: &Palette, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .w(px(design::SIDEBAR_WIDTH))
            .flex_shrink_0()
            .h_full()
            .bg(palette.surface)
            .border_r_1()
            .border_color(palette.border)
            .p_4()
            .flex()
            .flex_col()
            .gap_3()
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .py_3()
                    .child(
                        svg()
                            .path("brand/verifact-mark.svg")
                            .size(px(26.))
                            .text_color(palette.accent),
                    )
                    .child(
                        div()
                            .text_size(px(20.))
                            .font_weight(FontWeight::SEMIBOLD)
                            .child("Verifact"),
                    ),
            )
            .child(
                div()
                    .text_size(px(design::META_SIZE))
                    .text_color(palette.secondary)
                    .child("Your local testing workspace"),
            )
            .child(
                Button::new("add-project-sidebar")
                    .primary()
                    .child(design::action_label("Add project", palette, true))
                    .disabled(self.busy)
                    .tooltip("Choose a local project directory")
                    .on_click(cx.listener(|this, _, window, cx| this.pick_project(window, cx))),
            )
            .child(
                div()
                    .mt_4()
                    .text_size(px(design::META_SIZE))
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(palette.secondary)
                    .child("PROJECTS"),
            )
            .child(
                div()
                    .id("project-list")
                    .flex_1()
                    .overflow_y_scroll()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .when(self.projects.is_empty(), |element| {
                        element.child(
                            div()
                                .py_3()
                                .text_color(palette.muted)
                                .child("No projects added yet"),
                        )
                    })
                    .children(self.projects.iter().cloned().map(|project| {
                        let selected = self
                            .selected
                            .as_ref()
                            .is_some_and(|current| current.id == project.id)
                            && self.mode == WorkspaceMode::Projects;
                        Button::new(SharedString::from(format!("project-{}", project.id)))
                            .ghost()
                            .child(design::action_label(project.name.clone(), palette, false))
                            .bg(if selected {
                                palette.selected
                            } else {
                                palette.surface
                            })
                            .disabled(self.busy)
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.load_project(project.clone(), cx)
                            }))
                    })),
            )
            .child(
                Button::new("browser-workspace")
                    .ghost()
                    .child(design::action_label("Open browser", palette, false))
                    .on_click(cx.listener(|this, _, window, cx| this.show_browser(window, cx))),
            )
            .child(
                div()
                    .border_t_1()
                    .border_color(palette.border)
                    .pt_3()
                    .flex()
                    .flex_col()
                    .gap_2()
                    .child(
                        div()
                            .text_size(px(design::META_SIZE))
                            .text_color(palette.secondary)
                            .child("APPEARANCE"),
                    )
                    .child(
                        div()
                            .flex()
                            .gap_1()
                            .child(
                                Button::new("dark-theme")
                                    .ghost()
                                    .child(design::action_label("Dark", palette, false))
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        this.set_theme(ThemePreference::Dark, window, cx)
                                    })),
                            )
                            .child(
                                Button::new("light-theme")
                                    .ghost()
                                    .child(design::action_label("Light", palette, false))
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        this.set_theme(ThemePreference::Light, window, cx)
                                    })),
                            )
                            .child(
                                Button::new("system-theme")
                                    .ghost()
                                    .child(design::action_label("Auto", palette, false))
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        this.set_theme(ThemePreference::System, window, cx)
                                    })),
                            ),
                    ),
            )
            .child(
                div()
                    .text_size(px(design::META_SIZE))
                    .text_color(palette.muted)
                    .child("Local · No account required"),
            )
    }

    fn render_header(&self, palette: &Palette, cx: &mut Context<Self>) -> impl IntoElement {
        let browser = self.browser.clone();
        let browser_forward = self.browser.clone();
        let browser_reload = self.browser.clone();
        let title = if self.mode == WorkspaceMode::Browser {
            if cfg!(debug_assertions) {
                "Browser · Temporary session".into()
            } else {
                "Browser".into()
            }
        } else {
            self.selected
                .as_ref()
                .map(|project| project.name.clone())
                .unwrap_or_else(|| "Your projects".into())
        };
        div()
            .h(px(design::HEADER_HEIGHT))
            .flex_shrink_0()
            .px_6()
            .py_3()
            .border_b_1()
            .border_color(palette.border)
            .flex()
            .flex_col()
            .gap_2()
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .child(
                        div()
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_size(px(design::TITLE_SIZE))
                            .child(title),
                    )
                    .when_some(
                        self.error
                            .clone()
                            .filter(|_| self.mode == WorkspaceMode::Browser),
                        |element, error| {
                            element.child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .overflow_hidden()
                                    .text_size(px(design::META_SIZE))
                                    .text_color(palette.failure)
                                    .child(error),
                            )
                        },
                    )
                    .when(
                        self.mode == WorkspaceMode::Projects && self.project_base_url.is_some(),
                        |element| {
                            element.child(
                                Button::new("open-project-site")
                                    .child(design::action_label("Open site", palette, false))
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        this.open_project_site(window, cx)
                                    })),
                            )
                        },
                    )
                    .when(
                        self.mode == WorkspaceMode::Projects && self.selected.is_some(),
                        |element| {
                            element.child(
                                Button::new("rescan")
                                    .child(design::action_label(
                                        if self.busy {
                                            "Scanning…"
                                        } else {
                                            "Rescan project"
                                        },
                                        palette,
                                        false,
                                    ))
                                    .disabled(self.busy || self.selected.is_none())
                                    .on_click(
                                        cx.listener(|this, _, _, cx| this.start_scan(None, cx)),
                                    ),
                            )
                        },
                    ),
            )
            .when(self.mode == WorkspaceMode::Browser, |element| {
                element.child(
                    div()
                        .flex()
                        .items_center()
                        .gap_2()
                        .child(
                            Button::new("back")
                                .ghost()
                                .child(design::action_label("Back", palette, false))
                                .on_click(move |_, _, _| {
                                    if let Some(browser) = browser.borrow().as_ref() {
                                        browser.back();
                                    }
                                }),
                        )
                        .child(
                            Button::new("forward")
                                .ghost()
                                .child(design::action_label("Forward", palette, false))
                                .on_click(move |_, _, _| {
                                    if let Some(browser) = browser_forward.borrow().as_ref() {
                                        browser.forward();
                                    }
                                }),
                        )
                        .child(
                            Button::new("reload")
                                .ghost()
                                .child(design::action_label("Reload", palette, false))
                                .on_click(move |_, _, _| {
                                    if let Some(browser) = browser_reload.borrow().as_ref() {
                                        browser.reload();
                                    }
                                }),
                        )
                        .child(div().flex_1().child(Input::new(&self.address)))
                        .child(
                            Button::new("navigate")
                                .primary()
                                .child(design::action_label("Go", palette, true))
                                .on_click(cx.listener(|this, _, _, cx| this.navigate(cx))),
                        ),
                )
            })
            .when(self.mode == WorkspaceMode::Projects, |element| {
                element.child(
                    div()
                        .text_color(palette.secondary)
                        .text_size(px(design::META_SIZE))
                        .child(
                            self.selected
                                .as_ref()
                                .map(|project| project.path.display().to_string())
                                .unwrap_or_else(|| {
                                    "Connect a local codebase to discover its testing tools.".into()
                                }),
                        ),
                )
            })
    }

    fn render_project(&self, palette: &Palette, cx: &mut Context<Self>) -> AnyElement {
        if self.selected.is_none() {
            return div().flex_1().flex().flex_col().justify_center().items_center().gap_4().p_8()
                .child(svg().path("brand/verifact-mark.svg").size(px(56.)).text_color(palette.accent))
                .child(div().text_size(px(design::TITLE_SIZE)).font_weight(FontWeight::SEMIBOLD).child("Start with a project"))
                .child(div().max_w(px(440.)).text_color(palette.secondary).text_center().child("Add a local PHP or Node.js codebase. Verifact will inspect its files and show the test infrastructure already there."))
                .child(Button::new("add-project-empty").primary().child(design::action_label("Add project", palette, true)).on_click(cx.listener(|this, _, window, cx| this.pick_project(window, cx))))
                .child(div().text_size(px(design::META_SIZE)).text_color(palette.muted).child("Discovery reads files. It never runs project code.")).into_any_element();
        }
        let Some(report) = &self.report else {
            return div()
                .flex_1()
                .p_8()
                .text_color(palette.secondary)
                .child(if self.busy {
                    "Inspecting project files…"
                } else {
                    "No saved scan yet. Rescan this project to build its catalogue."
                })
                .into_any_element();
        };

        let details = report.clone();
        let mut content = div().flex_1().min_h_0().p_6().flex().flex_col().gap_4()
            .child(div().flex().items_center().justify_between()
                .child(div().flex().flex_col().gap_1()
                    .child(div().font_weight(FontWeight::SEMIBOLD).text_size(px(design::TITLE_SIZE)).child(format!("{} candidate test files", report.candidates.len())))
                    .child(div().text_color(palette.secondary).text_size(px(design::META_SIZE)).child(format!("{} detected framework configurations · No tests run", report.frameworks.len()))))
                .child(Button::new("scan-details").ghost().child(design::action_label(if self.details_open { "Hide scan details" } else { "Scan details" }, palette, false))
                    .on_click(cx.listener(|this, _, _, cx| { this.details_open = !this.details_open; cx.notify(); }))))
            .child(div().flex().gap_3().items_center()
                .child(div().flex_1().child(Input::new(&self.search)))
                .child(Button::new("all-frameworks").ghost().child(design::action_label("All frameworks", palette, false))
                    .on_click(cx.listener(|this, _, _, cx| { this.framework_filter = None; this.catalogue_dirty = true; cx.notify(); }))))
            .child(div().flex().gap_2().flex_wrap().children(report.frameworks.iter().map(|framework| {
                let id = framework.id.clone();
                let active = self.framework_filter.as_ref() == Some(&framework.id);
                Button::new(SharedString::from(format!("filter-{}", framework.id))).ghost()
                    .bg(if active { palette.selected } else { palette.canvas })
                    .child(design::action_label(format!("{} · {}", framework.name, framework.package_path.display()), palette, false))
                    .on_click(cx.listener(move |this, _, _, cx| { this.framework_filter = Some(id.clone()); this.catalogue_dirty = true; cx.notify(); }))
            })))
            .child(div().text_color(palette.muted).text_size(px(design::META_SIZE)).child("Files are discovery candidates, not confirmed individual test cases. Open a file to inspect its source."));
        if self.details_open {
            content = content.child(
                div()
                    .id("scan-details-content")
                    .max_h(px(230.))
                    .overflow_y_scroll()
                    .p_4()
                    .bg(palette.surface)
                    .rounded_md()
                    .flex()
                    .flex_col()
                    .gap_2()
                    .child(format!(
                        "Last scan: {} · {} files inspected",
                        details.scanned_at, details.files_inspected
                    ))
                    .child(format!(
                        "Revision: {} · Working-tree state not measured",
                        details.revision.as_deref().unwrap_or("unavailable")
                    ))
                    .children(details.frameworks.iter().map(|framework| {
                        div()
                            .text_size(px(design::META_SIZE))
                            .text_color(palette.secondary)
                            .child(format!(
                                "{} [{}]: {}",
                                framework.name,
                                framework.package_path.display(),
                                framework.evidence.join("; ")
                            ))
                    }))
                    .children(details.warnings.iter().map(|warning| {
                        div()
                            .text_size(px(design::META_SIZE))
                            .text_color(palette.warning)
                            .child(format!("{}: {}", warning.path.display(), warning.message))
                    }))
                    .children(self.jobs.iter().take(5).map(|job| {
                        div()
                            .text_size(px(design::META_SIZE))
                            .text_color(palette.secondary)
                            .child(format!(
                                "{} · {} · {}",
                                job.started_at, job.status, job.message
                            ))
                    })),
            );
        }
        content = content.child(div().flex().flex_1().min_h_0().gap_4()
            .child(div().flex_1().min_w_0().min_h_0().border_1().border_color(palette.border).rounded_md().overflow_hidden().child(self.catalogue.clone()))
            .when(self.selected_candidate.is_some(), |element| {
                let candidate = report.candidates.iter().find(|candidate| Some(&candidate.id) == self.selected_candidate.as_ref());
                if let Some(candidate) = candidate {
                    let path = self.selected.as_ref().map(|project| project.path.join(&candidate.file));
                    element.child(div().w(px(270.)).flex_shrink_0().bg(palette.surface).rounded_md().p_4().flex().flex_col().gap_3()
                        .child(div().font_weight(FontWeight::SEMIBOLD).child("Candidate file"))
                        .child(div().font_family(design::FONT_CODE).text_size(px(design::META_SIZE)).child(candidate.file.display().to_string()))
                        .child(div().text_color(palette.secondary).child("Not run · File candidate"))
                        .child(div().text_size(px(design::META_SIZE)).text_color(palette.secondary).child("This file has not been enumerated or executed. Its test-case count and result are unknown."))
                        .child(Button::new("open-source").child(design::action_label("Open source file", palette, false)).on_click(move |_, _, cx| {
                            if let Some(path) = &path { cx.open_with_system(path); }
                        })))
                } else { element }
            }));
        content.into_any_element()
    }
}

impl Render for Workspace {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let palette = self.palette(window);
        div()
            .key_context("Verifact")
            .track_focus(&self.focus)
            .on_action(
                cx.listener(|this, _: &AddProject, window, cx| this.pick_project(window, cx)),
            )
            .on_action(cx.listener(|this, _: &FocusSearch, window, cx| {
                if this.mode == WorkspaceMode::Projects {
                    this.search.update(cx, |input, cx| input.focus(window, cx));
                }
            }))
            .on_action(cx.listener(|this, _: &FocusAddress, window, cx| {
                if this.mode == WorkspaceMode::Browser {
                    this.address.update(cx, |input, cx| input.focus(window, cx));
                }
            }))
            .size_full()
            .flex()
            .font_family(design::FONT_UI)
            .text_size(px(design::BODY_SIZE))
            .bg(palette.canvas)
            .text_color(palette.text)
            .when(!self.browser_only, |element| {
                element.child(self.render_sidebar(&palette, cx))
            })
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .h_full()
                    .flex()
                    .flex_col()
                    .child(self.render_header(&palette, cx))
                    .when_some(self.activity.clone(), |element, message| {
                        element.child(
                            div()
                                .px_6()
                                .py_2()
                                .bg(palette.selected)
                                .text_color(palette.info)
                                .child(message),
                        )
                    })
                    .when_some(
                        self.error
                            .clone()
                            .filter(|_| self.mode == WorkspaceMode::Projects),
                        |element, message| {
                            element.child(
                                div()
                                    .px_6()
                                    .py_2()
                                    .bg(palette.surface)
                                    .text_color(palette.failure)
                                    .child(message),
                            )
                        },
                    )
                    .when(self.mode == WorkspaceMode::Projects, |element| {
                        element.child(self.render_project(&palette, cx))
                    }),
            )
    }
}
