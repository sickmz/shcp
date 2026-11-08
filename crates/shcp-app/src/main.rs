#![cfg_attr(windows, windows_subsystem = "windows")]

mod actions;
mod assets;
mod clipboard;
mod files_format;
mod files_view;
mod login_prompt;
mod modals;
mod motion;
mod notices;
mod panel;
mod ports_panel;
mod session_overlay;
mod sidebar;
mod state;
mod tabs;
mod terminal_element;
mod terminal_input;
mod terminal_layout;
mod terminal_panes;
mod terminal_view;
mod theme;
mod title_bar;
mod transfers_panel;
mod updater;
mod widgets;
#[cfg(windows)]
mod windows;
mod workspace;

use gpui::{
    Application, Bounds, TitlebarOptions, WindowBounds, WindowOptions, point, prelude::*, px, size,
};
use shcp_core::Event;
use state::AppState;
use workspace::Workspace;

/// Least of a saved window that must be on a screen for it to go back there: a monitor may be gone.
const MIN_ON_SCREEN: f32 = 100.;

/// Where the window was, while a good part of it is on some screen; otherwise centered.
fn initial_bounds(saved: Option<shcp_core::SavedWindow>, cx: &gpui::App) -> WindowBounds {
    let default = || WindowBounds::Windowed(Bounds::centered(None, size(px(1280.), px(800.)), cx));
    let Some(saved) = saved else {
        return default();
    };
    let bounds = Bounds::new(
        point(px(saved.x), px(saved.y)),
        size(px(saved.width.max(480.)), px(saved.height.max(320.))),
    );
    let visible = cx.displays().iter().any(|display| {
        let shared = display.bounds().intersect(&bounds).size;
        f32::from(shared.width) >= MIN_ON_SCREEN && f32::from(shared.height) >= MIN_ON_SCREEN
    });
    match (visible, saved.maximized) {
        (false, _) => default(),
        (true, true) => WindowBounds::Maximized(bounds),
        (true, false) => WindowBounds::Windowed(bounds),
    }
}

/// Tells the user, as there is no console. Logging may be what failed.
fn show_fatal_error(text: &str) {
    #[cfg(windows)]
    windows::error_box(text);
    #[cfg(not(windows))]
    eprintln!("{text}");
}

/// A crash of the interface ends the app: the user is told where the details are. A crash in a
/// background task only ends that task, and stays in the log.
fn tell_about_crashes(log_dir: &std::path::Path) {
    let crash_log = log_dir.join(shcp_core::CRASH_LOG);
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        previous(info);
        if std::thread::current().name() == Some("main") {
            show_fatal_error(&format!(
                "shcp ran into a problem and has to close.\n\nThe details are in {}",
                crash_log.display()
            ));
        }
    }));
}

fn main() {
    #[cfg(windows)]
    {
        let after_update = std::env::args().any(|arg| arg == updater::AFTER_UPDATE);
        if !windows::first_instance(after_update) {
            windows::focus_running_window();
            return;
        }
    }
    let (engine, events) = match shcp_core::start(None) {
        Ok(started) => started,
        Err(error) => {
            show_fatal_error(&format!("Could not start shcp: {error:#}"));
            std::process::exit(1);
        }
    };
    tell_about_crashes(&engine.paths().log_dir);
    let shutdown_engine = engine.clone();

    Application::new()
        .with_assets(assets::Assets)
        .run(move |cx| {
            let themes_dir = engine.paths().config_dir.join("themes");
            let (catalog, mut theme_errors) = theme::ThemeCatalog::load(&themes_dir);
            cx.set_global(catalog);
            let icon_themes_dir = engine.paths().config_dir.join("icon-themes");
            let (icons, icon_errors) = theme::icon_theme::IconThemeCatalog::load(&icon_themes_dir);
            cx.set_global(icons);
            theme_errors.extend(icon_errors);
            theme::load_fonts(cx);
            theme::apply(&engine.settings(), &engine, cx);
            actions::bind_keys(&engine.settings(), cx);
            cx.on_window_closed(|cx| {
                if cx.windows().is_empty() {
                    cx.quit();
                }
            })
            .detach();

            let state = cx.new(|cx| {
                let mut state = AppState::new(engine);
                for error in theme_errors {
                    state.push_notice(shcp_core::NoticeLevel::Important, error, cx);
                }
                if let Some(name) = theme::missing_theme(&state.settings, cx) {
                    state.push_notice(
                        shcp_core::NoticeLevel::Info,
                        format!(
                            "The theme \"{name}\" is not installed. \
                             Find it under Settings, Appearance, Browse themes"
                        ),
                        cx,
                    );
                }
                state
            });
            let window_bounds = initial_bounds(state.read(cx).settings.window, cx);
            let window_background = theme::window_background(&state.read(cx).settings);
            let opened = cx.open_window(
                WindowOptions {
                    window_bounds: Some(window_bounds),
                    window_background,
                    titlebar: Some(TitlebarOptions {
                        title: Some("shcp".into()),
                        // Windows gets our own title bar, with the caption buttons inside it.
                        appears_transparent: cfg!(windows),
                        ..Default::default()
                    }),
                    ..Default::default()
                },
                {
                    let state = state.clone();
                    move |window, cx| {
                        cx.new(|cx| {
                            let mut workspace = Workspace::new(state, window, cx);
                            workspace.restore_tabs(window, cx);
                            workspace
                        })
                    }
                },
            );
            if let Err(error) = opened {
                show_fatal_error(&format!("Could not open the window: {error:#}"));
                cx.quit();
                return;
            }

            state.update(cx, |state, cx| state.start_update_checks(cx));

            // Core events → AppState. Drain whatever is queued in one update.
            cx.spawn(async move |cx| {
                while let Ok(first) = events.recv().await {
                    let mut batch: Vec<Event> = vec![first];
                    while let Ok(next) = events.try_recv() {
                        batch.push(next);
                    }
                    let alive = state.update(cx, |state, cx| {
                        for event in batch {
                            state.apply(event, cx);
                        }
                    });
                    if alive.is_err() {
                        break;
                    }
                }
            })
            .detach();
            cx.activate(true);
        });
    // The window is gone, but the runtime must finish disconnecting before the process exits.
    shutdown_engine.shutdown();
}
