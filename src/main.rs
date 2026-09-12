// Widget builder chains are deep expression trees; keep the macro
// expansion budget generous.
#![recursion_limit = "512"]

mod app;
mod views;
mod widgets;

fn main() {
    // Headless --version for installers and CI smoke tests: must exit before
    // the platform application opens, which needs no display server.
    // Every arm below exits, so only the first argument is ever inspected.
    if let Some(arg) = std::env::args().nth(1) {
        match arg.as_str() {
            "--version" | "-V" => {
                println!("pimon {}", env!("CARGO_PKG_VERSION"));
                return;
            }
            "--help" | "-h" => {
                println!("Usage: pimon [--version]");
                return;
            }
            _ => {
                eprintln!("pimon: unrecognized argument: {arg}\nUsage: pimon [--version]");
                std::process::exit(2);
            }
        }
    }

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
