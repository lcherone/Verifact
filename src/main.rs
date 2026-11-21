mod assets;
mod browser_host;
mod catalogue;
pub mod design;
mod mac_application;
mod showcase;
mod workspace;

use anyhow::Result;
use gpui::{App, AppContext, Application, Bounds, WindowBounds, WindowOptions, px, size};
use gpui_component::Root;
use verifact_core::{Core, default_data_dir};

fn main() -> Result<()> {
    let core = Core::open(default_data_dir()?)?;
    let recovered = core.store()?.recover_interrupted_jobs()?;
    eprintln!(
        "{}",
        serde_json::json!({"event":"verifact_started","data_dir":core.data_dir(),"recovered_jobs":recovered})
    );
    let executable = std::env::current_exe()?;
    let loader = cef::library_loader::LibraryLoader::new(&executable, false);
    anyhow::ensure!(loader.load(), "Unable to load bundled CEF framework");
    let _ = cef::api_hash(cef::sys::CEF_API_VERSION_LAST, 0);
    mac_application::prepare()?;
    let application = Application::new().with_assets(assets::Assets::default());
    let port = std::env::var("VERIFACT_CDP_PORT")
        .unwrap_or_else(|_| "9227".into())
        .parse()?;
    let browser_only = std::env::var("VERIFACT_BROWSER_ONLY").is_ok_and(|value| value == "1");
    let gallery = std::env::var("VERIFACT_DESIGN_SHOWCASE").is_ok_and(|value| value == "1");
    application.run(move |cx: &mut App| {
        gpui_component::init(cx);
        workspace::init(cx);
        let bounds = Bounds::centered(None, size(px(1240.), px(840.)), cx);
        let opened = cx.open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                window_min_size: Some(size(px(850.), px(580.))),
                titlebar: Some(gpui::TitlebarOptions {
                    title: Some("Verifact".into()),
                    ..Default::default()
                }),
                ..Default::default()
            },
            |window, cx| {
                if gallery {
                    let view = cx.new(|cx| showcase::Showcase::new(window, cx));
                    // The showcase does not create a CEF browser; allow ordinary close.
                    mac_application::allow_termination();
                    cx.new(|cx| Root::new(view, window, cx))
                } else {
                    let view = cx.new(|cx| {
                        match workspace::Workspace::new(
                            core.clone(),
                            browser_only,
                            port,
                            window,
                            cx,
                        ) {
                            Ok(workspace) => workspace,
                            Err(error) => {
                                eprintln!("VERIFACT_STARTUP_FAILED: {error:#}");
                                std::process::exit(1);
                            }
                        }
                    });
                    cx.new(|cx| Root::new(view, window, cx))
                }
            },
        );
        if let Err(error) = opened {
            eprintln!("GPUI_WINDOW_FAILED: {error:#}");
            mac_application::allow_termination();
            cx.quit();
        }
        cx.activate(true);
    });
    Ok(())
}
