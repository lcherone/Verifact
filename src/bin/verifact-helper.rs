fn main() -> anyhow::Result<()> {
    let executable = std::env::current_exe()?;
    let loader = cef::library_loader::LibraryLoader::new(&executable, true);
    anyhow::ensure!(loader.load(), "Unable to load CEF in helper");
    let _ = cef::api_hash(cef::sys::CEF_API_VERSION_LAST, 0);
    let args = cef::args::Args::new();
    let code = cef::execute_process(Some(args.as_main_args()), None, std::ptr::null_mut());
    if code > 0 {
        std::process::exit(code);
    }
    Ok(())
}
