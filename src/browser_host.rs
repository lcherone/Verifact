//! macOS native child-view adapter. Chromium draws into a real NSView and
//! receives Cocoa input directly; GPUI owns the surrounding native window.
use anyhow::{Context, Result, ensure};
use cef::*;
use cocoa::{
    base::{id, nil},
    foundation::{NSPoint, NSRect, NSSize},
};
use objc::{class, msg_send, sel, sel_impl};
use raw_window_handle::{HasWindowHandle, RawWindowHandle};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

wrap_life_span_handler! {
    struct Lifecycle { closed: Arc<AtomicBool>, }
    impl LifeSpanHandler {
        fn do_close(&self, _browser: Option<&mut Browser>) -> i32 {
            eprintln!("CEF_DO_CLOSE");
            0
        }
        fn on_before_close(&self, _browser: Option<&mut Browser>) {
            self.closed.store(true, Ordering::SeqCst);
            eprintln!("CEF_BROWSER_CLOSED");
        }
    }
}
wrap_client! {
    struct HostClient { closed: Arc<AtomicBool>, }
    impl Client {
        fn life_span_handler(&self) -> Option<LifeSpanHandler> {
            Some(Lifecycle::new(self.closed.clone()))
        }
    }
}
wrap_app! {
    struct BrowserApp { temporary_storage: bool, }
    impl App {
        fn on_before_command_line_processing(&self, _kind: Option<&CefString>, command: Option<&mut CommandLine>) {
            if let Some(command) = command {
                command.append_switch_with_value(Some(&"remote-debugging-address".into()), Some(&"127.0.0.1".into()));
                if self.temporary_storage {
                    // Only a newly created disposable development profile uses the
                    // testing keychain. Existing protected profiles are never opened
                    // under this switch or copied into a temporary session.
                    command.append_switch(Some(&"use-mock-keychain".into()));
                }
            }
        }
    }
}

struct BrowserStorage {
    path: std::path::PathBuf,
    temporary: Option<tempfile::TempDir>,
}

impl BrowserStorage {
    fn new(data_dir: &std::path::Path, temporary: bool) -> Result<Self> {
        std::fs::create_dir_all(data_dir)?;
        let data_dir = data_dir.canonicalize()?;
        if temporary {
            let sessions = data_dir.join("browser-sessions");
            std::fs::create_dir_all(&sessions)?;
            let mut builder = tempfile::Builder::new();
            builder.prefix("session-");
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                builder.permissions(std::fs::Permissions::from_mode(0o700));
            }
            let session = builder.tempdir_in(sessions)?;
            Ok(Self {
                path: session.path().to_owned(),
                temporary: Some(session),
            })
        } else {
            let path = data_dir.join("browser-profile");
            std::fs::create_dir_all(&path)?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700))?;
            }
            Ok(Self {
                path,
                temporary: None,
            })
        }
    }

    fn uses_testing_keychain(&self) -> bool {
        self.temporary.is_some()
    }
}

pub struct BrowserRuntime {
    // Retain the private temporary directory until CEF has finished using it.
    _storage: BrowserStorage,
}

impl BrowserRuntime {
    pub fn pump(&self) {
        cef::do_message_loop_work();
    }
    pub fn shutdown(self) {
        cef::shutdown();
        eprintln!("CEF_SHUTDOWN_COMPLETE");
    }
}

pub fn initialize_runtime(port: u16, data_dir: &std::path::Path) -> Result<BrowserRuntime> {
    let storage = BrowserStorage::new(data_dir, cfg!(debug_assertions))?;
    let logs = data_dir.canonicalize()?.join("logs");
    std::fs::create_dir_all(&logs)?;
    let args = args::Args::new();
    let mut app = BrowserApp::new(storage.uses_testing_keychain());
    let settings = Settings {
        command_line_args_disabled: 1,
        no_sandbox: 1, // Development host; production sandbox/signing are separate work.
        external_message_pump: 1,
        remote_debugging_port: i32::from(port),
        root_cache_path: storage.path.to_string_lossy().as_ref().into(),
        cache_path: storage
            .path
            .join("Default")
            .to_string_lossy()
            .as_ref()
            .into(),
        keychain_service_name: "Verifact Safe Storage".into(),
        keychain_account_name: "Verifact".into(),
        log_file: logs.join("cef.log").to_string_lossy().as_ref().into(),
        ..Default::default()
    };
    let initialized = initialize(
        Some(args.as_main_args()),
        Some(&settings),
        Some(&mut app),
        std::ptr::null_mut(),
    ) == 1;
    if !initialized {
        // CEF explicitly forbids retries or further API calls after a failed
        // initialize. GetExitCode is its only permitted exception.
        let code = cef::get_exit_code();
        eprintln!("CEF_INITIALIZATION_TERMINATED exit_code={code}");
        drop(storage);
        std::process::exit(code);
    }
    eprintln!(
        "{}",
        serde_json::json!({"event":"browser_runtime_started", "temporary_profile":storage.uses_testing_keychain()})
    );
    Ok(BrowserRuntime { _storage: storage })
}

pub struct BrowserHost {
    browser: Option<Browser>,
    container: id,
    pub closed: Arc<AtomicBool>,
    closing: bool,
}

impl BrowserHost {
    pub fn new(window: &gpui::Window, url: &str) -> Result<Self> {
        let RawWindowHandle::AppKit(handle) = HasWindowHandle::window_handle(window)?.as_raw()
        else {
            anyhow::bail!("This feasibility adapter requires macOS AppKit");
        };
        let parent = handle.ns_view.as_ptr() as id;
        // SAFETY: called on GPUI's main thread with its live NSView. The child is
        // retained here and by its parent; Drop releases our ownership after CEF closes.
        let container: id = unsafe {
            let bounds: NSRect = msg_send![parent, bounds];
            let frame = NSRect::new(
                NSPoint::new(0., 0.),
                NSSize::new(bounds.size.width, (bounds.size.height - 96.).max(1.)),
            );
            let view: id = msg_send![class!(NSView), alloc];
            let view: id = msg_send![view, initWithFrame: frame];
            ensure!(view != nil, "Unable to create browser container");
            let _: () = msg_send![view, setAutoresizingMask: 18usize]; // width + height
            let _: () = msg_send![parent, addSubview: view];
            view
        };
        let bounds = window.viewport_size();
        let info = WindowInfo {
            runtime_style: RuntimeStyle::ALLOY,
            ..Default::default()
        }
        .set_as_child(
            container.cast(),
            &Rect {
                x: 0,
                y: 0,
                width: f32::from(bounds.width) as i32,
                height: (f32::from(bounds.height) - 96.).max(1.) as i32,
            },
        );
        let closed = Arc::new(AtomicBool::new(false));
        let mut client = HostClient::new(closed.clone());
        let browser = browser_host_create_browser_sync(
            Some(&info),
            Some(&mut client),
            Some(&url.into()),
            Some(&BrowserSettings::default()),
            None,
            None,
        );
        let Some(browser) = browser else {
            unsafe {
                let _: () = msg_send![container, removeFromSuperview];
                let _: () = msg_send![container, release];
            }
            anyhow::bail!("CEF did not create a browser");
        };
        let host = browser.host().context("CEF browser has no host")?;
        unsafe {
            let child = host.window_handle() as id;
            ensure!(child != nil, "CEF did not create a native browser view");
            let _: () = msg_send![child, setAutoresizingMask: 18usize];
        }
        eprintln!(
            "CEF_BROWSER_CREATED id={} parent={container:p}",
            browser.identifier()
        );
        Ok(Self {
            browser: Some(browser),
            container,
            closed,
            closing: false,
        })
    }

    pub fn back(&self) {
        if let Some(browser) = &self.browser {
            browser.go_back();
        }
    }
    pub fn forward(&self) {
        if let Some(browser) = &self.browser {
            browser.go_forward();
        }
    }
    pub fn reload(&self) {
        if let Some(browser) = &self.browser {
            browser.reload();
        }
    }
    pub fn navigate(&self, url: &str) {
        if let Some(frame) = self
            .browser
            .as_ref()
            .and_then(|browser| browser.main_frame())
        {
            frame.load_url(Some(&url.into()));
        }
    }
    pub fn current_url(&self) -> Option<String> {
        self.browser
            .as_ref()
            .and_then(|browser| browser.main_frame())
            .map(|frame| CefString::from(&frame.url()).to_string())
    }

    pub fn set_visible(&self, visible: bool) {
        if self.container != nil {
            // SAFETY: all host methods run on GPUI's AppKit main thread.
            unsafe {
                let _: () = msg_send![self.container, setHidden: if visible { cocoa::base::NO } else { cocoa::base::YES }];
            }
        }
    }

    pub fn set_workspace_layout(&mut self, sidebar_width: f32, header_height: f32) {
        // SAFETY: container and its parent are live AppKit views on the main thread.
        unsafe {
            let parent: id = msg_send![self.container, superview];
            let bounds: NSRect = msg_send![parent, bounds];
            let frame = NSRect::new(
                NSPoint::new(f64::from(sidebar_width), 0.),
                NSSize::new(
                    (bounds.size.width - f64::from(sidebar_width)).max(1.),
                    (bounds.size.height - f64::from(header_height)).max(1.),
                ),
            );
            let _: () = msg_send![self.container, setFrame: frame];
        }
    }
    pub fn close(&mut self) {
        if !self.closing {
            self.closing = true;
            if let Some(host) = self.browser.as_ref().and_then(|browser| browser.host()) {
                host.close_browser(1);
            }
        }
    }
    pub fn is_closed(&self) -> bool {
        self.closed.load(Ordering::SeqCst)
    }
    pub fn can_close_window(&mut self) -> bool {
        let allowed = self
            .browser
            .as_ref()
            .and_then(|browser| browser.host())
            .is_none_or(|host| host.try_close_browser() == 1);
        eprintln!("CEF_WINDOW_CLOSE_ALLOWED={allowed}");
        if allowed {
            self.detach_container();
        }
        allowed
    }
    pub fn release_closed_browser(&mut self) {
        if self.is_closed() {
            self.browser.take();
        }
    }
    fn detach_container(&mut self) {
        if self.container != nil {
            unsafe {
                let _: () = msg_send![self.container, removeFromSuperview];
                let _: () = msg_send![self.container, release];
            }
            self.container = nil;
        }
    }
}

impl Drop for BrowserHost {
    fn drop(&mut self) {
        // The main loop waits for on_before_close before dropping the host.
        self.detach_container();
    }
}

#[cfg(test)]
mod storage_tests {
    use super::*;

    #[test]
    fn disposable_sessions_are_unique_and_leave_protected_storage_untouched() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let protected = BrowserStorage::new(directory.path(), false)?;
        std::fs::write(protected.path.join("sentinel"), b"protected data")?;
        let first = BrowserStorage::new(directory.path(), true)?;
        let second = BrowserStorage::new(directory.path(), true)?;
        assert!(first.uses_testing_keychain());
        assert!(second.uses_testing_keychain());
        assert_ne!(first.path, second.path);
        assert!(!protected.uses_testing_keychain());
        assert!(protected.path.is_absolute());
        assert!(first.path.is_absolute());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(&first.path)?.permissions().mode() & 0o777,
                0o700
            );
            assert_eq!(
                std::fs::metadata(&protected.path)?.permissions().mode() & 0o777,
                0o700
            );
        }
        let first_path = first.path.clone();
        drop(first);
        assert!(!first_path.exists());
        assert_eq!(
            std::fs::read(protected.path.join("sentinel"))?,
            b"protected data"
        );
        Ok(())
    }
}
