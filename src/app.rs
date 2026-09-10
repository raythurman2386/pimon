use std::borrow::Cow;
use std::collections::VecDeque;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use gpui_kit::component::{Root, Theme, ThemeMode};
use gpui_kit::*;
use notify::{EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use pimon::sensors::{Clock, Probe, Snapshot, SystemProbe};
use pimon::theme::{omarchy_watch_paths, OmarchyPalette};

use crate::views::render_pimon;

pub const WINDOW_WIDTH: f32 = 720.;
pub const WINDOW_HEIGHT: f32 = 640.;
/// One sweep per second; sysfs reads are effectively free, vcgencmd is a
/// few milliseconds, and 1 Hz keeps the Pi's CPU out of the measurements.
pub const POLL_INTERVAL: Duration = Duration::from_millis(1000);
/// Roughly five minutes of 1 Hz history.
pub const MAX_HISTORY: usize = 300;

actions!(pimon_actions, [Quit]);

pub fn init(cx: &mut App) {
    load_fonts(cx);
    cx.bind_keys([KeyBinding::new("ctrl-q", Quit, None)]);
    cx.on_action(|_: &Quit, cx| cx.quit());
}

fn load_fonts(cx: &mut App) {
    let fonts: [&'static [u8]; 4] = [
        include_bytes!("../fonts/iAWriterMonoS-Regular.ttf"),
        include_bytes!("../fonts/iAWriterMonoS-Italic.ttf"),
        include_bytes!("../fonts/iAWriterMonoS-Bold.ttf"),
        include_bytes!("../fonts/iAWriterMonoS-BoldItalic.ttf"),
    ];
    let blobs = fonts.into_iter().map(Cow::Borrowed).collect::<Vec<_>>();
    let _ = cx.text_system().add_fonts(blobs);
}

pub fn open_window(cx: &AsyncApp) -> anyhow::Result<WindowHandle<Root>> {
    cx.open_window(window_options(), move |window, cx| {
        window.activate_window();
        window.set_window_title("Pimon");
        let view: Entity<Pimon> = cx.new(|cx| Pimon::new(window, cx));
        cx.new(|cx| {
            let any_view: AnyView = view.into();
            Root::new(any_view, window, cx)
        })
    })
    .map_err(|e| anyhow::anyhow!("{e}"))
}

fn window_options() -> WindowOptions {
    WindowOptions {
        window_bounds: Some(WindowBounds::Windowed(Bounds {
            origin: point(px(120.), px(120.)),
            // Client-side chrome is drawn inside the viewport like the rest
            // of the suite; the design size leaves room for it.
            size: size(px(WINDOW_WIDTH), px(WINDOW_HEIGHT)),
        })),
        window_min_size: Some(size(px(560.), px(420.))),
        titlebar: Some(TitlebarOptions {
            title: Some("Pimon".into()),
            appears_transparent: false,
            traffic_light_position: None,
        }),
        app_id: Some("pimon".into()),
        ..Default::default()
    }
}

/// Health of the board, derived from the throttle bitmask and peak temp.
#[derive(Clone, Copy, PartialEq)]
pub enum Health {
    /// Nothing has happened since boot and temps are in range.
    Ok,
    /// Something happened since boot but nothing is active now.
    Warn,
    /// A throttle condition is live right now or a temp is past the line.
    Alert,
}

/// The live-updating sensor dashboard.
pub struct Pimon {
    sweeper: pimon::sweep::Sweeper,
    probe: Arc<dyn Probe + Send + Sync>,
    clock: Clock,
    history: VecDeque<Snapshot>,
    active_tab: usize,
    palette: OmarchyPalette,
    theme_watch: ThemeWatch,
    /// Set when the very first sweep can't find vcgencmd at all, so the
    /// UI can explain what is missing instead of showing empty charts.
    probe_error: Option<String>,
}

impl Pimon {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let probe: Arc<dyn Probe + Send + Sync> = Arc::new(SystemProbe::new());
        let palette = OmarchyPalette::load(true);
        let mut this = Self {
            sweeper: pimon::sweep::Sweeper::new(),
            probe,
            clock: Clock::started(),
            history: VecDeque::with_capacity(MAX_HISTORY),
            active_tab: 0,
            palette,
            theme_watch: ThemeWatch::new(),
            probe_error: None,
        };
        apply_palette(&this.palette, Some(window), cx);
        this.sweep(cx);
        this.start_polling(cx);
        this
    }

    fn start_polling(&mut self, cx: &mut Context<Self>) {
        cx.spawn(async move |this, cx| loop {
            smol::Timer::after(POLL_INTERVAL).await;
            // `update` returns () here; the entity is gone when the window
            // closes, which ends the loop.
            let Ok(()) = this.update(cx, |this, cx| {
                this.sweep(cx);
                cx.notify();
            }) else {
                break;
            };
        })
        .detach();
    }

    /// One sensor sweep into the history ring; rate metrics come from the
    /// stateful Sweeper's counter deltas.
    fn sweep(&mut self, cx: &mut Context<Self>) {
        let snapshot = self.sweeper.sweep(self.probe.as_ref(), &self.clock);
        match &snapshot.error {
            Some(err) => self.probe_error = Some(err.clone()),
            None => self.probe_error = None,
        }
        if self.history.len() >= MAX_HISTORY {
            self.history.pop_front();
        }
        self.history.push_back(snapshot);

        // Omarchy replaces its theme dir with rm -rf + mv, dropping inotify
        // watches; rearm whenever a watched path disappeared.
        if self.theme_watch.needs_rearm() {
            self.theme_watch.watch_all(omarchy_watch_paths());
        }
        if !self.theme_watch.drain().is_empty() {
            let fresh = OmarchyPalette::load(self.palette.dark);
            if fresh != self.palette {
                self.palette = fresh;
                apply_palette(&self.palette, None, cx);
            }
        }
    }

    pub fn latest(&self) -> Option<&Snapshot> {
        self.history.back()
    }

    pub fn history(&self) -> &VecDeque<Snapshot> {
        &self.history
    }

    pub fn probe_error(&self) -> Option<&str> {
        self.probe_error.as_deref()
    }

    pub fn active_tab(&self) -> usize {
        self.active_tab
    }

    pub fn set_active_tab(&mut self, index: usize, _cx: &mut Context<Self>) {
        self.active_tab = index;
    }

    /// The health badge state for the current snapshot.
    pub fn health(&self) -> Health {
        let Some(latest) = self.latest() else {
            return Health::Ok;
        };
        if latest.throttle.active_now() {
            return Health::Alert;
        }
        if let Some(temp) = latest.max_temp_c() {
            // Firmware's soft limit for the Pi 5 family is 85 C by default;
            // warn well before it, alert at the line itself.
            if temp >= 85. {
                return Health::Alert;
            }
            if temp >= 75. {
                return Health::Warn;
            }
        }
        if latest.throttle.any_event() {
            return Health::Warn;
        }
        Health::Ok
    }
}

/// A notify watcher on the Omarchy theme paths, copied from the pi-suite
/// pattern: Omarchy `rm -rf` + `mv`s `current/theme`, so watches are
/// rearmed when a path disappears.
struct ThemeWatch {
    events: Arc<std::sync::Mutex<Vec<std::path::PathBuf>>>,
    watcher: Option<RecommendedWatcher>,
    watched: Vec<std::path::PathBuf>,
}

impl ThemeWatch {
    fn new() -> Self {
        let events = Arc::new(std::sync::Mutex::new(Vec::new()));
        let tx = events.clone();
        let watcher = RecommendedWatcher::new(
            move |res: notify::Result<notify::Event>| {
                if let Ok(event) = res {
                    if matches!(
                        event.kind,
                        EventKind::Modify(_) | EventKind::Remove(_) | EventKind::Create(_)
                    ) {
                        if let Ok(mut queue) = tx.lock() {
                            queue.extend(event.paths);
                        }
                    }
                }
            },
            notify::Config::default(),
        )
        .ok();
        let mut this = Self {
            events,
            watcher,
            watched: Vec::new(),
        };
        this.watch_all(omarchy_watch_paths());
        this
    }

    fn watch_all(&mut self, paths: impl IntoIterator<Item = std::path::PathBuf>) {
        self.unwatch_all();
        for path in paths {
            self.watch_path(&path);
        }
    }

    fn watch_path(&mut self, path: &Path) {
        if !path.exists() || self.watched.iter().any(|p| p == path) {
            return;
        }
        if let Some(watcher) = self.watcher.as_mut() {
            if watcher.watch(path, RecursiveMode::NonRecursive).is_ok() {
                self.watched.push(path.to_path_buf());
            }
        }
    }

    fn unwatch_all(&mut self) {
        if let Some(watcher) = self.watcher.as_mut() {
            for path in self.watched.drain(..) {
                let _ = watcher.unwatch(&path);
            }
        } else {
            self.watched.clear();
        }
    }

    fn drain(&self) -> Vec<std::path::PathBuf> {
        self.events
            .lock()
            .map(|mut q| q.drain(..).collect())
            .unwrap_or_default()
    }

    fn needs_rearm(&self) -> bool {
        self.watched.iter().any(|path| !path.exists())
    }
}

pub fn apply_palette(palette: &OmarchyPalette, window: Option<&mut Window>, cx: &mut App) {
    Theme::change(
        if palette.dark {
            ThemeMode::Dark
        } else {
            ThemeMode::Light
        },
        window,
        cx,
    );
    let theme = Theme::global_mut(cx);
    if let Some(bg) = crate::views::hex_to_hsla(&palette.background) {
        theme.colors.background = bg;
    }
    if let Some(fg) = crate::views::hex_to_hsla(&palette.foreground) {
        theme.colors.foreground = fg;
    }
    if let Some(accent) = crate::views::hex_to_hsla(&palette.accent) {
        theme.colors.primary = accent;
        theme.colors.accent = accent;
    }
    theme.mono_font_family = "iA Writer Mono S".into();
    theme.mono_font_size = px(13.);
    Theme::sync_base(cx);
}

impl Render for Pimon {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        render_pimon(self, window, cx)
    }
}
