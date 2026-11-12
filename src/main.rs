mod appearance;
mod config;
mod completion;
mod connection;
mod connection_form;
mod runtime;
mod query_panel;
mod results;
mod table_preview;
mod workspace;

use std::borrow::Cow;

use gpui_kit::*;

use workspace::Workspace;

gpui_kit::assets::icon_assets!(ExtraIcons, [Server, Database, Table, ArrowLeft, ChevronLeft]);

/// The component library's default icon bundle plus the few extra Lucide
/// icons this app uses (only the listed SVGs end up in the binary).
struct AppAssets;

impl AssetSource for AppAssets {
    fn load(&self, path: &str) -> Result<Option<Cow<'static, [u8]>>> {
        match ExtraIcons.load(path)? {
            Some(bytes) => Ok(Some(bytes)),
            None => gpui_kit::assets::Assets.load(path),
        }
    }

    fn list(&self, path: &str) -> Result<Vec<SharedString>> {
        let mut paths = gpui_kit::assets::Assets.list(path)?;
        paths.extend(ExtraIcons.list(path)?);
        Ok(paths)
    }
}

fn main() {
    gpui_kit::application()
        .with_assets(AppAssets)
        .run(|cx| {
            gpui_kit::init(cx);
            appearance::init(cx);
            query_panel::init(cx);

            let options = WindowOptions {
                window_bounds: Some(WindowBounds::centered(size(px(1100.), px(720.)), cx)),
                titlebar: Some(TitlebarOptions {
                    title: Some("mysqlgpui".into()),
                    ..Default::default()
                }),
                ..Default::default()
            };
            gpui_kit::open_window(options, cx, |window, cx| cx.new(|cx| Workspace::new(window, cx)))
                .expect("failed to open window");
            cx.activate(true);
        });
}
