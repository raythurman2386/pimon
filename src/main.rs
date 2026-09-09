mod app;
mod views;

fn main() {
    let app = gpui_kit::application().with_assets(gpui_kit::assets::Assets);
    app.run(|cx| {
        gpui_kit::init(cx);
        app::init(cx);
        cx.activate(true);

        cx.spawn(async move |cx| {
            app::open_window(cx).expect("failed to open window");
        })
        .detach();
    });
}
