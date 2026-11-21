fn main() -> anyhow::Result<()> {
    let info = cef::build_util::mac::BundleInfo::new(
        "Verifact",
        "local.verifact.desktop",
        "Verifact",
        "English",
        "0.1.0".parse()?,
    );
    let path = cef::build_util::mac::bundle(
        std::path::Path::new("target/bundle"),
        std::path::Path::new("target/debug"),
        "verifact",
        "verifact-helper",
        None,
        info,
    )?;
    let branded_path = std::path::Path::new("target/bundle/Verifact.app");
    std::fs::rename(&path, branded_path)?;
    let path = branded_path;
    let resources = path.join("Contents/Resources");
    std::fs::create_dir_all(&resources)?;
    std::fs::copy(
        "assets/brand/Verifact.icns",
        resources.join("Verifact.icns"),
    )?;
    std::fs::copy(
        "target/debug/verifact-cli",
        path.join("Contents/MacOS/verifact-cli"),
    )?;
    let manifest = path.join("Contents/Info.plist");
    let mut metadata = plist::Value::from_file(&manifest)?;
    let dictionary = metadata
        .as_dictionary_mut()
        .ok_or_else(|| anyhow::anyhow!("Invalid application Info.plist"))?;
    dictionary.insert("CFBundleIconFile".into(), "Verifact.icns".into());
    dictionary.insert("LSMinimumSystemVersion".into(), "13.0".into());
    metadata.to_file_xml(manifest)?;
    // cef-rs currently reuses the main identifier for all helpers. Give each
    // bundle its own identity so Launch Services and native automation can
    // distinguish the UI application from Chromium subprocesses.
    for entry in std::fs::read_dir(path.join("Contents/Frameworks"))? {
        let helper = entry?.path();
        if helper
            .extension()
            .is_some_and(|extension| extension == "app")
        {
            let info_path = helper.join("Contents/Info.plist");
            let mut value = plist::Value::from_file(&info_path)?;
            let dictionary = value
                .as_dictionary_mut()
                .ok_or_else(|| anyhow::anyhow!("Invalid helper Info.plist"))?;
            let name = helper
                .file_stem()
                .and_then(|name| name.to_str())
                .ok_or_else(|| anyhow::anyhow!("Invalid helper bundle name"))?;
            let suffix: String = name.chars().filter(char::is_ascii_alphanumeric).collect();
            dictionary.insert(
                "CFBundleIdentifier".into(),
                format!("local.verifact.desktop.{suffix}").into(),
            );
            dictionary.insert(
                "CFBundleDisplayName".into(),
                name.replace("verifact", "Verifact").into(),
            );
            dictionary.insert(
                "CFBundleName".into(),
                name.replace("verifact", "Verifact").into(),
            );
            dictionary.insert("LSMinimumSystemVersion".into(), "13.0".into());
            value.to_file_xml(info_path)?;
        }
    }
    println!("{}", path.display());
    Ok(())
}
