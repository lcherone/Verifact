//! App-owned vector branding plus the matching component library's icon set.
use gpui::{AssetSource, SharedString};
use std::borrow::Cow;

#[derive(rust_embed::RustEmbed)]
#[folder = "assets/"]
struct BrandAssets;

pub struct Assets {
    component: gpui_component_assets::Assets,
}
impl Default for Assets {
    fn default() -> Self {
        Self {
            component: gpui_component_assets::Assets,
        }
    }
}
impl AssetSource for Assets {
    fn load(&self, path: &str) -> gpui::Result<Option<Cow<'static, [u8]>>> {
        if let Some(asset) = BrandAssets::get(path) {
            return Ok(Some(asset.data));
        }
        self.component.load(path)
    }
    fn list(&self, path: &str) -> gpui::Result<Vec<SharedString>> {
        let mut assets = self.component.list(path)?;
        assets.extend(
            BrandAssets::iter()
                .filter(|name| name.starts_with(path))
                .map(|name| SharedString::from(name.into_owned())),
        );
        Ok(assets)
    }
}
