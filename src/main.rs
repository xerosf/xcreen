#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod modules;

use ::image::ImageFormat;
use modules::config::AppConfig;
use modules::monitor::{LevelKind, MonitorInfo, MonitorLevels};
use std::cell::RefCell;
use std::collections::VecDeque;
use std::sync::atomic::{AtomicIsize, Ordering};
use std::sync::{Condvar, Mutex, MutexGuard, OnceLock};
use std::thread;
use std::time::{Duration, Instant};
use tracing::{error, info, warn};
use tray_icon::{
    Icon, MouseButton, MouseButtonState, TrayIcon, TrayIconBuilder, TrayIconEvent,
    menu::{Menu, MenuItem, PredefinedMenuItem},
};
use windows::Win32::Foundation::{BOOL, HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows::Win32::Graphics::Dwm::{DWMWINDOWATTRIBUTE, DwmSetWindowAttribute};
use windows::Win32::Graphics::Gdi::{
    GetMonitorInfoW, MONITOR_DEFAULTTONEAREST, MONITORINFO, MonitorFromPoint,
};
use windows::Win32::System::Threading::GetCurrentThreadId;
use windows::Win32::UI::HiDpi::{GetDpiForMonitor, GetDpiForWindow, MDT_EFFECTIVE_DPI};
use windows::Win32::UI::Input::KeyboardAndMouse::{GetAsyncKeyState, VK_ESCAPE};
use windows::Win32::UI::Shell::{DefSubclassProc, SetWindowSubclass};
use windows::Win32::UI::WindowsAndMessaging::{
    EnumThreadWindows, GA_ROOTOWNER, GWL_EXSTYLE, GWL_STYLE, GetAncestor, GetForegroundWindow,
    GetWindowLongW, GetWindowRect, GetWindowThreadProcessId, HWND_TOPMOST, IsWindowVisible,
    KillTimer, PostMessageW, SW_HIDE, SW_SHOW, SWP_FRAMECHANGED, SWP_NOACTIVATE, SWP_NOMOVE,
    SWP_NOSIZE, SetForegroundWindow, SetTimer, SetWindowLongW, SetWindowPos, ShowWindow, WM_APP,
    WM_CANCELMODE, WM_DISPLAYCHANGE, WM_POWERBROADCAST, WM_TIMER, WS_CAPTION, WS_EX_APPWINDOW,
    WS_EX_TOOLWINDOW, WS_MAXIMIZEBOX, WS_MINIMIZEBOX, WS_SYSMENU, WS_THICKFRAME,
};
use windows_reactor::{
    App, AsyncSetState, Backdrop, ComboBox, Component, Element, ElementExt, GridLength,
    HorizontalAlignment, InfoBar, InfoBarSeverity, InnerConstraints, ProgressRing, ReactorHost,
    RenderCx, Slider, Thickness, ToggleSwitch, VerticalAlignment, WindowSize, body, body_strong,
    border, button, caption, grid, hstack, subtitle, vstack,
};
use winreg::RegKey;
use winreg::enums::{HKEY_CURRENT_USER, KEY_SET_VALUE};

const FLYOUT_WIDTH_DIP: f64 = 360.0;
const FLYOUT_HEIGHT_DIP: f64 = 280.0;
/// The click that opens the flyout also moves foreground. Hiding on that
/// focus change would close the window before it is visible.
const FLYOUT_DISMISS_GRACE: Duration = Duration::from_millis(400);
const WM_SHOW_FLYOUT: u32 = WM_APP + 1;
const WM_HIDE_FLYOUT: u32 = WM_APP + 2;
const DISMISS_TIMER: usize = 0x5843_0001;
const DISCOVERY_TIMER: usize = 0x5843_0002;
const PBT_APMRESUMEAUTOMATIC: usize = 0x12;

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ActiveView {
    Main,
    Settings,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct UiMonitor {
    id: String,
    name: String,
    is_primary: bool,
    ready: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct UiSnapshot {
    monitors: Vec<UiMonitor>,
    selected: usize,
    selection_explicit: bool,
    levels: Option<MonitorLevels>,
    loading: bool,
    refreshing: bool,
    ambient_available: bool,
    error: Option<String>,
    load_generation: u64,
    current_view: ActiveView,
    autostart_enabled: bool,
    log_level: String,
}

impl UiSnapshot {
    fn empty(ambient_available: bool) -> Self {
        Self {
            monitors: Vec::new(),
            selected: 0,
            selection_explicit: false,
            levels: None,
            loading: false,
            refreshing: false,
            ambient_available,
            error: None,
            load_generation: 0,
            current_view: ActiveView::Main,
            autostart_enabled: false,
            log_level: "warn".to_string(),
        }
    }

    fn selected_monitor(&self) -> Option<&UiMonitor> {
        self.monitors.get(self.selected)
    }
}

static UI_SNAPSHOT: OnceLock<Mutex<UiSnapshot>> = OnceLock::new();
static UI_SETTER: OnceLock<AsyncSetState<UiSnapshot>> = OnceLock::new();
static CONFIG: OnceLock<Mutex<AppConfig>> = OnceLock::new();
static FLYOUT_HWND: AtomicIsize = AtomicIsize::new(0);
static SHOWN_AT: Mutex<Option<Instant>> = Mutex::new(None);

thread_local! {
    static RUNTIME: RefCell<Option<(ReactorHost, TrayIcon)>> = const { RefCell::new(None) };
}

fn shared_snapshot() -> &'static Mutex<UiSnapshot> {
    UI_SNAPSHOT
        .get()
        .expect("invariant: UI snapshot is initialized before callbacks")
}

fn shared_config() -> &'static Mutex<AppConfig> {
    CONFIG
        .get()
        .expect("invariant: config is initialized before callbacks")
}

fn publish_snapshot() {
    let snapshot = lock(shared_snapshot());
    if let Some(setter) = UI_SETTER.get() {
        setter.call(snapshot.clone());
    }
}

fn choose_selected_index(
    monitors: &[UiMonitor],
    preserve_id: Option<&str>,
    explicit: bool,
) -> usize {
    let previous = preserve_id.and_then(|id| monitors.iter().position(|monitor| monitor.id == id));
    previous
        .filter(|&index| explicit || monitors[index].ready)
        .or_else(|| {
            monitors
                .iter()
                .position(|monitor| monitor.is_primary && monitor.ready)
        })
        .or_else(|| monitors.iter().position(|monitor| monitor.ready))
        .or(previous)
        .unwrap_or(0)
}

#[derive(Debug)]
enum MonitorCommand {
    Refresh,
    Read {
        id: String,
        generation: u64,
    },
    Apply {
        id: String,
        levels: MonitorLevels,
        generation: u64,
    },
    Ambient {
        id: String,
        generation: u64,
    },
}

#[derive(Default)]
struct MonitorQueue {
    commands: Mutex<VecDeque<(MonitorCommand, Instant)>>,
    wake: Condvar,
}

impl MonitorQueue {
    fn retry_discovery(&self) {
        let mut commands = lock(&self.commands);
        if !commands
            .iter()
            .any(|(cmd, _)| matches!(cmd, MonitorCommand::Refresh))
        {
            commands.push_back((
                MonitorCommand::Refresh,
                Instant::now() + Duration::from_secs(1),
            ));
            self.wake.notify_one();
        }
    }

    fn send(&self, command: MonitorCommand) {
        let mut commands = lock(&self.commands);
        match &command {
            MonitorCommand::Refresh => {
                commands.retain(|(cmd, _)| !matches!(cmd, MonitorCommand::Refresh))
            }
            MonitorCommand::Read { .. } => {
                commands.retain(|(cmd, _)| !matches!(cmd, MonitorCommand::Read { .. }))
            }
            MonitorCommand::Apply {
                id,
                levels,
                generation,
            } => {
                if let Some((
                    MonitorCommand::Apply {
                        id: last_id,
                        levels: last,
                        generation: last_gen,
                    },
                    due,
                )) = commands.iter_mut().rev().find(|(cmd, _)| match cmd {
                    MonitorCommand::Apply { id: queued_id, .. } => queued_id == id,
                    _ => false,
                }) && id == last_id
                {
                    if levels.brightness.is_some() {
                        last.brightness = levels.brightness;
                    }
                    if levels.contrast.is_some() {
                        last.contrast = levels.contrast;
                    }
                    *last_gen = *generation;
                    *due = Instant::now() + Duration::from_millis(100);
                    self.wake.notify_one();
                    return;
                }
            }
            MonitorCommand::Ambient { id, .. } => {
                commands.retain(|(cmd, _)| !matches!(cmd, MonitorCommand::Ambient { id: queued_id, .. } | MonitorCommand::Apply { id: queued_id, .. } if queued_id == id));
            }
        }
        let delay = if matches!(command, MonitorCommand::Apply { .. }) {
            Duration::from_millis(100)
        } else {
            Duration::ZERO
        };
        commands.push_back((command, Instant::now() + delay));
        self.wake.notify_one();
    }

    fn receive(&self) -> MonitorCommand {
        let mut commands = lock(&self.commands);
        loop {
            if let Some((_, due)) = commands.front() {
                let remaining = due.saturating_duration_since(Instant::now());
                if remaining.is_zero() {
                    return commands
                        .pop_front()
                        .expect("invariant: queue front exists while locked")
                        .0;
                }
                (commands, _) = self
                    .wake
                    .wait_timeout(commands, remaining)
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
            } else {
                commands = self
                    .wake
                    .wait(commands)
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
            }
        }
    }
}

static MONITOR_QUEUE: OnceLock<MonitorQueue> = OnceLock::new();

fn send_monitor_command(command: MonitorCommand) {
    MONITOR_QUEUE
        .get_or_init(MonitorQueue::default)
        .send(command);
}

fn refresh_monitors_async(explicit: bool) {
    {
        let mut snapshot = lock(shared_snapshot());
        snapshot.refreshing |= explicit;
        snapshot.loading = snapshot.levels.is_none();
    }
    publish_snapshot();
    send_monitor_command(MonitorCommand::Refresh);
}

fn sync_monitor_snapshot(snapshot: &mut UiSnapshot, monitors: &[MonitorInfo], generation: u64) {
    let selected_id = snapshot
        .selected_monitor()
        .map(|monitor| monitor.id.clone());
    snapshot.monitors = monitors
        .iter()
        .map(|monitor| UiMonitor {
            id: monitor.id.clone(),
            name: monitor.name.clone(),
            is_primary: monitor.is_primary,
            ready: monitor.levels.is_some(),
        })
        .collect();
    snapshot.selected = choose_selected_index(
        &snapshot.monitors,
        selected_id.as_deref(),
        snapshot.selection_explicit,
    );
    let selected = monitors.get(snapshot.selected);
    let selection_changed = selected.map(|monitor| monitor.id.as_str()) != selected_id.as_deref();
    if selection_changed {
        snapshot.selection_explicit = false;
    }
    if selection_changed || snapshot.load_generation == generation {
        snapshot.levels = selected.and_then(|monitor| monitor.levels);
        snapshot.error = selected.and_then(|monitor| monitor.level_error.clone());
        snapshot.loading = false;
    }
    snapshot.refreshing = false;
}

fn start_monitor_worker() -> std::io::Result<()> {
    thread::Builder::new()
        .name("monitor-control".into())
        .spawn(|| {
            let mut monitors = Vec::new();
            let mut startup_retries = 2;
            loop {
                let command = MONITOR_QUEUE.get_or_init(MonitorQueue::default).receive();
                if matches!(command, MonitorCommand::Refresh) {
                    let generation = lock(shared_snapshot()).load_generation;
                    match modules::monitor::get_monitor_list_sync() {
                        Ok(discovered) => {
                            monitors = discovered;
                            sync_monitor_snapshot(
                                &mut lock(shared_snapshot()),
                                &monitors,
                                generation,
                            );
                        }
                        Err(err) => {
                            let mut snapshot = lock(shared_snapshot());
                            snapshot.loading = false;
                            snapshot.refreshing = false;
                            snapshot.error = Some(err);
                        }
                    }
                    if startup_retries > 0
                        && (monitors.is_empty()
                            || monitors.iter().any(|monitor| monitor.levels.is_none()))
                    {
                        startup_retries -= 1;
                        MONITOR_QUEUE
                            .get_or_init(MonitorQueue::default)
                            .retry_discovery();
                    } else {
                        startup_retries = 0;
                    }
                    publish_snapshot();
                    continue;
                }
                let (id, generation) = match &command {
                    MonitorCommand::Read { id, generation }
                    | MonitorCommand::Apply { id, generation, .. }
                    | MonitorCommand::Ambient { id, generation } => (id, *generation),
                    MonitorCommand::Refresh => unreachable!("refresh handled above"),
                };
                // Reads superseded by navigation or a slider edit need no hardware work.
                if matches!(command, MonitorCommand::Read { .. })
                    && lock(shared_snapshot()).load_generation != generation
                {
                    continue;
                }
                let monitor = monitors.iter_mut().find(|monitor| monitor.id == *id);
                let result = match monitor {
                    Some(monitor) => {
                        let result = match &command {
                            MonitorCommand::Read { .. } => {
                                modules::monitor::read_monitor_levels(monitor)
                                    .map(|levels| monitor.levels = Some(levels))
                            }
                            MonitorCommand::Apply { levels, .. } => {
                                modules::monitor::apply_levels(monitor, *levels)
                            }
                            MonitorCommand::Ambient { .. } => {
                                modules::sensor::get_light_sensor_lux().and_then(|lux| {
                                    let mut requested = ambient_levels(lux);
                                    if let Some(supported) = monitor.levels {
                                        if supported.brightness.is_none() {
                                            requested.brightness = None;
                                        }
                                        if supported.contrast.is_none() {
                                            requested.contrast = None;
                                        }
                                    }
                                    modules::monitor::apply_levels(monitor, requested)
                                })
                            }
                            MonitorCommand::Refresh => unreachable!("refresh handled above"),
                        };
                        monitor.level_error = result.err();
                        (monitor.levels, monitor.level_error.clone())
                    }
                    None => (
                        None,
                        Some("The selected monitor is no longer connected".into()),
                    ),
                };
                let mut snapshot = lock(shared_snapshot());
                if snapshot.load_generation == generation
                    && snapshot.selected_monitor().map(|m| m.id.as_str()) == Some(id.as_str())
                {
                    snapshot.levels = result.0;
                    snapshot.error = result.1;
                    snapshot.loading = false;
                }
                drop(snapshot);
                publish_snapshot();
            }
        })?;
    Ok(())
}

fn ambient_levels(lux: f64) -> MonitorLevels {
    let (brightness, contrast) = match lux {
        x if x < 10.0 => (20, 40),
        x if x < 50.0 => (30, 45),
        x if x < 100.0 => (40, 55),
        x if x < 200.0 => (50, 65),
        x if x < 500.0 => (65, 75),
        x if x < 1000.0 => (80, 85),
        _ => (100, 95),
    };
    MonitorLevels {
        brightness: Some(brightness),
        contrast: Some(contrast),
    }
}

fn load_selected_levels() {
    let mut snapshot = lock(shared_snapshot());
    let Some(id) = snapshot
        .selected_monitor()
        .map(|monitor| monitor.id.clone())
    else {
        return;
    };
    snapshot.load_generation += 1;
    snapshot.loading = snapshot.levels.is_none();
    let generation = snapshot.load_generation;
    drop(snapshot);
    publish_snapshot();
    send_monitor_command(MonitorCommand::Read { id, generation });
}

fn apply_ambient(id: String) {
    let mut snapshot = lock(shared_snapshot());
    snapshot.load_generation += 1;
    let generation = snapshot.load_generation;
    drop(snapshot);
    send_monitor_command(MonitorCommand::Ambient { id, generation });
}

fn navigate_monitor(delta: isize) {
    let mut snapshot = lock(shared_snapshot());
    let count = snapshot.monitors.len();
    if count == 0 {
        return;
    }
    snapshot.selection_explicit = true;
    snapshot.selected = (snapshot.selected as isize + delta).rem_euclid(count as isize) as usize;
    snapshot.levels = None;
    snapshot.error = None;
    drop(snapshot);
    load_selected_levels();
}

fn update_local_level(kind: LevelKind, value: u32) {
    let mut snapshot = lock(shared_snapshot());
    let Some(id) = snapshot
        .selected_monitor()
        .map(|monitor| monitor.id.clone())
    else {
        return;
    };
    let Some(mut levels) = snapshot.levels else {
        return;
    };
    let channel = match kind {
        LevelKind::Brightness => &mut levels.brightness,
        LevelKind::Contrast => &mut levels.contrast,
    };
    if *channel == Some(value) || channel.is_none() {
        return;
    }
    *channel = Some(value);
    snapshot.levels = Some(levels);
    snapshot.load_generation += 1;
    let generation = snapshot.load_generation;
    let requested = match kind {
        LevelKind::Brightness => MonitorLevels {
            brightness: Some(value),
            contrast: None,
        },
        LevelKind::Contrast => MonitorLevels {
            brightness: None,
            contrast: Some(value),
        },
    };
    drop(snapshot);
    publish_snapshot();
    send_monitor_command(MonitorCommand::Apply {
        id,
        levels: requested,
        generation,
    });
}

fn apply_settings_to_snapshot(autostart_enabled: bool, log_level: &str) {
    let mut snapshot = lock(shared_snapshot());
    snapshot.autostart_enabled = autostart_enabled;
    snapshot.log_level = log_level.to_string();
}

fn settings_are_open() -> bool {
    lock(shared_snapshot()).current_view == ActiveView::Settings
}

fn update_setting(change: impl FnOnce(&mut AppConfig)) {
    let mut current = lock(shared_config());
    let mut next = current.clone();
    change(&mut next);
    if next == *current {
        return;
    }
    let result = next
        .validate()
        .and_then(|()| set_autostart(next.autostart_enabled))
        .and_then(|()| next.save());
    if let Err(err) = result {
        if let Err(rollback) = set_autostart(current.autostart_enabled) {
            error!("Autostart rollback failed: {rollback}");
        }
        drop(current);
        lock(shared_snapshot()).error = Some(err);
    } else {
        modules::logger::apply_log_level(&next.log_level);
        apply_settings_to_snapshot(next.autostart_enabled, &next.log_level);
        *current = next;
        drop(current);
    }
    publish_snapshot();
}

fn update_autostart(enable: bool) {
    update_setting(|config| config.autostart_enabled = enable);
}

fn update_log_level(level: String) {
    update_setting(|config| config.log_level = level);
}

fn app(cx: &mut RenderCx) -> Element {
    let initial = lock(shared_snapshot());
    let (snapshot, setter) = cx.use_async_state(initial.clone());
    UI_SETTER.get_or_init(|| setter);
    drop(initial);

    let main_inner: Element = if snapshot.monitors.is_empty() {
        vstack((
            subtitle(if snapshot.loading {
                "Discovering monitors…"
            } else {
                "No compatible monitors"
            }),
            body(if snapshot.loading {
                "Reading connected displays."
            } else {
                "Connect a DDC/CI monitor, then refresh."
            }),
            button("Refresh")
                .enabled(!snapshot.loading)
                .on_click(|| refresh_monitors_async(true)),
        ))
        .spacing(12.0)
        .into()
    } else {
        let monitor = snapshot
            .selected_monitor()
            .expect("invariant: selection indexes a nonempty list");
        let count = snapshot.monitors.len();
        let header = grid((
            body_strong(if monitor.is_primary {
                format!("{} (Primary)", monitor.name)
            } else {
                monitor.name.clone()
            })
            .grid_column(0)
            .horizontal_alignment(HorizontalAlignment::Left)
            .vertical_alignment(VerticalAlignment::Center),
            hstack((
                button("\u{E76B}")
                    .font_family("Segoe Fluent Icons")
                    .font_size(16.0)
                    .width(32.0)
                    .height(32.0)
                    .padding(0.0)
                    .subtle()
                    .enabled(count > 1 && !snapshot.loading)
                    .on_click(|| navigate_monitor(-1))
                    .automation_name("Previous monitor"),
                button("\u{E76C}")
                    .font_family("Segoe Fluent Icons")
                    .font_size(16.0)
                    .width(32.0)
                    .height(32.0)
                    .padding(0.0)
                    .subtle()
                    .enabled(count > 1 && !snapshot.loading)
                    .on_click(|| navigate_monitor(1))
                    .automation_name("Next monitor"),
            ))
            .spacing(4.0)
            .grid_column(1)
            .horizontal_alignment(HorizontalAlignment::Right),
        ))
        .columns([GridLength::STAR, GridLength::Auto])
        .horizontal_alignment(HorizontalAlignment::Stretch);

        let controls: Element = if snapshot.loading && snapshot.levels.is_none() {
            vstack((
                ProgressRing::indeterminate(),
                body("Reading monitor controls…"),
            ))
            .spacing(8.0)
            .into()
        } else if let Some(levels) = snapshot.levels {
            vstack((
                vstack((
                    body_strong("Brightness"),
                    Slider::new(levels.brightness.unwrap_or(0) as f64)
                        .enabled(levels.brightness.is_some())
                        .range(0.0, 100.0)
                        .step(1.0)
                        .on_value_changed(|value: f64| {
                            update_local_level(LevelKind::Brightness, value.round() as u32)
                        })
                        .automation_name("Brightness")
                        .horizontal_alignment(HorizontalAlignment::Stretch),
                ))
                .spacing(6.0),
                vstack((
                    body_strong("Contrast"),
                    Slider::new(levels.contrast.unwrap_or(0) as f64)
                        .enabled(levels.contrast.is_some())
                        .range(0.0, 100.0)
                        .step(1.0)
                        .on_value_changed(|value: f64| {
                            update_local_level(LevelKind::Contrast, value.round() as u32)
                        })
                        .automation_name("Contrast")
                        .horizontal_alignment(HorizontalAlignment::Stretch),
                ))
                .spacing(6.0),
            ))
            .spacing(16.0)
            .into()
        } else {
            body("Monitor levels are unavailable.").into()
        };

        vstack((header, controls)).spacing(12.0).into()
    };

    let settings_content: Element = {
        let header = grid((hstack((
            button("\u{E76B}")
                .font_family("Segoe Fluent Icons")
                .font_size(16.0)
                .width(32.0)
                .height(32.0)
                .padding(0.0)
                .subtle()
                .on_click(|| {
                    lock(shared_snapshot()).current_view = ActiveView::Main;
                    publish_snapshot();
                })
                .automation_name("Back to main"),
            body_strong("Settings").vertical_alignment(VerticalAlignment::Center),
        ))
        .spacing(8.0)
        .grid_column(0)
        .horizontal_alignment(HorizontalAlignment::Left),))
        .columns([GridLength::STAR, GridLength::Auto])
        .horizontal_alignment(HorizontalAlignment::Stretch);

        let controls = vstack((
            ToggleSwitch::new(snapshot.autostart_enabled)
                .header("Start with Windows")
                .on_toggled(|value: bool| {
                    if settings_are_open() {
                        update_autostart(value);
                    }
                })
                .horizontal_alignment(HorizontalAlignment::Stretch),
            ComboBox::new(vec!["Error", "Warn", "Info", "Debug"])
                .header("Log level")
                .selected_index(match snapshot.log_level.to_lowercase().as_str() {
                    "error" => 0,
                    "info" => 2,
                    "debug" => 3,
                    _ => 1,
                })
                .on_selection_changed(|index: i32| {
                    if !settings_are_open() {
                        return;
                    }
                    let level = match index {
                        0 => "error",
                        2 => "info",
                        3 => "debug",
                        _ => "warn",
                    };
                    update_log_level(level.to_string());
                })
                .horizontal_alignment(HorizontalAlignment::Stretch),
            vstack((
                body_strong(format!("XCreen v{}", env!("CARGO_PKG_VERSION"))),
                caption("MIT License • © 2026 xerosf. All rights reserved."),
            ))
            .spacing(4.0)
            .margin(Thickness {
                left: 0.0,
                top: 16.0,
                right: 0.0,
                bottom: 0.0,
            })
            .horizontal_alignment(HorizontalAlignment::Left),
        ))
        .spacing(12.0);

        vstack((header, controls)).spacing(12.0).into()
    };

    let ambient_id = snapshot
        .selected_monitor()
        .map(|monitor| monitor.id.clone());
    let ambient_enabled = snapshot.ambient_available && ambient_id.is_some();

    let refresh_button: Element = if snapshot.refreshing {
        ProgressRing::indeterminate().into()
    } else {
        button("\u{E72C}")
            .font_family("Segoe Fluent Icons")
            .font_size(16.0)
            .width(32.0)
            .height(32.0)
            .padding(0.0)
            .subtle()
            .tooltip("Refresh monitors")
            .automation_name("Refresh monitors")
            .on_click(|| refresh_monitors_async(true))
            .into()
    };

    let footer = grid((
        hstack((
            refresh_button,
            button("\u{E793}")
                .font_family("Segoe Fluent Icons")
                .font_size(16.0)
                .width(32.0)
                .height(32.0)
                .padding(0.0)
                .subtle()
                .enabled(ambient_enabled)
                .tooltip(if snapshot.ambient_available {
                    "Set from ambient light"
                } else {
                    "No ambient light sensor is available"
                })
                .automation_name("Set from ambient light")
                .on_click(move || {
                    if let Some(id) = ambient_id.clone() {
                        apply_ambient(id);
                    }
                }),
        ))
        .spacing(8.0)
        .grid_column(0)
        .horizontal_alignment(HorizontalAlignment::Left),
        button("\u{E713}")
            .font_family("Segoe Fluent Icons")
            .font_size(16.0)
            .width(32.0)
            .height(32.0)
            .padding(0.0)
            .subtle()
            .tooltip("Settings")
            .on_click(|| {
                lock(shared_snapshot()).current_view = ActiveView::Settings;
                publish_snapshot();
            })
            .grid_column(1)
            .horizontal_alignment(HorizontalAlignment::Right),
    ))
    .columns([GridLength::Auto, GridLength::STAR]);

    let error_element: Element = snapshot
        .error
        .as_ref()
        .map(|message| {
            InfoBar::new("Monitor error")
                .message(message)
                .severity(InfoBarSeverity::Error)
                .into()
        })
        .unwrap_or_else(|| vstack(()).into());

    let main_content: Element = grid((
        vstack((main_inner, error_element))
            .spacing(16.0)
            .grid_row(0),
        footer.grid_row(1),
    ))
    .rows(vec![GridLength::STAR, GridLength::Auto])
    .row_spacing(16.0)
    .into();

    let gap = 40.0;
    let content_width = 360.0 - 32.0; // 328.0
    let slide_amount = content_width + gap;
    let left_margin = if snapshot.current_view == ActiveView::Settings {
        -slide_amount
    } else {
        0.0
    };

    let main_opacity = if snapshot.current_view == ActiveView::Main {
        1.0
    } else {
        0.0
    };
    let settings_opacity = if snapshot.current_view == ActiveView::Settings {
        1.0
    } else {
        0.0
    };

    let body: Element = grid((
        main_content
            .width(content_width)
            .opacity(main_opacity)
            .with_opacity_transition(Duration::from_millis(200))
            .grid_column(0),
        settings_content
            .width(content_width)
            .opacity(settings_opacity)
            .with_opacity_transition(Duration::from_millis(200))
            .grid_column(2),
    ))
    .columns([
        GridLength::Pixel(content_width),
        GridLength::Pixel(gap),
        GridLength::Pixel(content_width),
    ])
    .width(content_width * 2.0 + gap)
    .margin(Thickness {
        left: left_margin,
        top: 0.0,
        right: 0.0,
        bottom: 0.0,
    })
    .with_translation_transition(Duration::from_millis(200))
    .into();

    border(body)
        .padding(Thickness {
            left: 16.0,
            top: 16.0,
            right: 16.0,
            bottom: 16.0,
        })
        .into()
}

fn set_autostart(enable: bool) -> std::result::Result<(), String> {
    let hkcu = RegKey::predef(HKEY_CURRENT_USER);
    if enable {
        let (key, _) = hkcu
            .create_subkey(r"SOFTWARE\Microsoft\Windows\CurrentVersion\Run")
            .map_err(|e| format!("Failed to open registry: {e}"))?;
        let exe_path =
            std::env::current_exe().map_err(|e| format!("Failed to get exe path: {e}"))?;
        key.set_value("XCreen", &format!("\"{}\"", exe_path.display()))
            .map_err(|e| format!("Failed to set registry value: {e}"))?;
    } else {
        match hkcu.open_subkey_with_flags(
            r"SOFTWARE\Microsoft\Windows\CurrentVersion\Run",
            KEY_SET_VALUE,
        ) {
            Ok(key) => match key.delete_value("XCreen") {
                Ok(()) => {}
                Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
                Err(err) => return Err(format!("Failed to remove autostart: {err}")),
            },
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
            Err(err) => return Err(format!("Failed to open autostart registry: {err}")),
        }
    }
    Ok(())
}

fn load_icon() -> std::result::Result<Icon, Box<dyn std::error::Error>> {
    let image =
        ::image::load_from_memory_with_format(include_bytes!("icons/32x32.png"), ImageFormat::Png)?;
    let rgba = image.to_rgba8();
    let (width, height) = rgba.dimensions();
    Ok(Icon::from_rgba(rgba.into_raw(), width, height)?)
}

unsafe extern "system" fn find_thread_window(hwnd: HWND, lparam: LPARAM) -> BOOL {
    let mut title_buf = [0u16; 512];
    // SAFETY: hwnd comes from EnumThreadWindows; the supplied title buffer is writable.
    let len =
        unsafe { windows::Win32::UI::WindowsAndMessaging::GetWindowTextW(hwnd, &mut title_buf) };
    let title = String::from_utf16_lossy(&title_buf[..len as usize]);

    if title == "XCreenFlyout" {
        // SAFETY: lparam points to the HWND local in current_thread_window during enumeration.
        unsafe {
            *(lparam.0 as *mut HWND) = hwnd;
        }
        return BOOL(0); // Stop enumeration
    }

    BOOL(1) // Continue enumeration
}

fn current_thread_window() -> Option<HWND> {
    let mut hwnd = HWND::default();
    // SAFETY: Enumeration is synchronous and hwnd remains writable until it returns.
    unsafe {
        let _ = EnumThreadWindows(
            GetCurrentThreadId(),
            Some(find_thread_window),
            LPARAM(&mut hwnd as *mut HWND as isize),
        );
    }
    (hwnd.0 != 0).then_some(hwnd)
}

fn configure_flyout_window(hwnd: HWND) {
    // SAFETY: hwnd is the owned flyout on its UI thread; DWM attribute pointers
    // have matching sizes and remain valid throughout their synchronous calls.
    unsafe {
        let style = GetWindowLongW(hwnd, GWL_STYLE) as u32;
        let remove = (WS_CAPTION | WS_THICKFRAME | WS_MINIMIZEBOX | WS_MAXIMIZEBOX | WS_SYSMENU).0;
        SetWindowLongW(hwnd, GWL_STYLE, (style & !remove) as i32);
        let exstyle = GetWindowLongW(hwnd, GWL_EXSTYLE) as u32;
        SetWindowLongW(
            hwnd,
            GWL_EXSTYLE,
            ((exstyle | WS_EX_TOOLWINDOW.0) & !WS_EX_APPWINDOW.0) as i32,
        );
        let _ = SetWindowPos(
            hwnd,
            HWND::default(),
            0,
            0,
            0,
            0,
            SWP_NOSIZE | SWP_NOACTIVATE | SWP_FRAMECHANGED,
        );

        // Force the same rounded DWM chrome and subtle system border used by
        // native Windows 11 flyouts. Unsupported attributes are harmless on
        // older Windows builds.
        let corner_preference = 2i32; // DWMWCP_ROUND
        let _ = DwmSetWindowAttribute(
            hwnd,
            DWMWINDOWATTRIBUTE(33), // DWMWA_WINDOW_CORNER_PREFERENCE
            &corner_preference as *const i32 as *const _,
            std::mem::size_of_val(&corner_preference) as u32,
        );
        let border_color = 0xffff_ffffu32; // DWMWA_COLOR_DEFAULT
        let _ = DwmSetWindowAttribute(
            hwnd,
            DWMWINDOWATTRIBUTE(34), // DWMWA_BORDER_COLOR
            &border_color as *const u32 as *const _,
            std::mem::size_of_val(&border_color) as u32,
        );
    }
}

fn get_monitor_dpi(hmonitor: windows::Win32::Graphics::Gdi::HMONITOR) -> u32 {
    let mut dpi_x = 0;
    let mut dpi_y = 0;
    // SAFETY: Both output pointers reference live u32 values. Invalid monitors return an error.
    unsafe {
        if GetDpiForMonitor(hmonitor, MDT_EFFECTIVE_DPI, &mut dpi_x, &mut dpi_y).is_ok() {
            dpi_x
        } else {
            96
        }
    }
}

fn position_flyout(hwnd: HWND, anchor_x: i32, anchor_y: i32) {
    // SAFETY: hwnd belongs to this UI thread, output structures are initialized,
    // and monitor information is checked before using its bounds.
    unsafe {
        let monitor = MonitorFromPoint(
            POINT {
                x: anchor_x,
                y: anchor_y,
            },
            MONITOR_DEFAULTTONEAREST,
        );
        let mut info = MONITORINFO {
            cbSize: std::mem::size_of::<MONITORINFO>() as u32,
            ..Default::default()
        };
        if !GetMonitorInfoW(monitor, &mut info).as_bool() {
            return;
        }
        let dpi = get_monitor_dpi(monitor).max(96);
        let width = (FLYOUT_WIDTH_DIP * dpi as f64 / 96.0).round() as i32;
        let height = (FLYOUT_HEIGHT_DIP * dpi as f64 / 96.0).round() as i32;

        // Resize the window first to let OS apply constraints
        let _ = SetWindowPos(
            hwnd,
            HWND_TOPMOST,
            0,
            0,
            width,
            height,
            SWP_NOACTIVATE | SWP_NOMOVE,
        );

        // Retrieve actual sized bounds
        let mut rect = RECT::default();
        let _ = GetWindowRect(hwnd, &mut rect);
        let actual_width = rect.right - rect.left;
        let actual_height = rect.bottom - rect.top;

        let screen = info.rcWork;
        let gap = (12.0 * dpi as f64 / 96.0).round() as i32;

        let (x, y) = calculate_flyout_position(screen, actual_width, actual_height, gap);
        let _ = SetWindowPos(
            hwnd,
            HWND_TOPMOST,
            x,
            y,
            actual_width,
            actual_height,
            SWP_NOACTIVATE,
        );
    }
}

fn calculate_flyout_position(screen: RECT, width: i32, height: i32, gap: i32) -> (i32, i32) {
    let x =
        (screen.right - width - gap).clamp(screen.left, (screen.right - width).max(screen.left));
    let y =
        (screen.bottom - height - gap).clamp(screen.top, (screen.bottom - height).max(screen.top));
    (x, y)
}

fn dismiss_tray_menu() {
    // SAFETY: The PID output is live, and the posted message has no pointer payload.
    unsafe {
        let fg = GetForegroundWindow();
        if fg.0 != 0 {
            let mut pid = 0;
            let _ = GetWindowThreadProcessId(fg, Some(&mut pid));
            if pid == std::process::id() {
                let _ = PostMessageW(fg, WM_CANCELMODE, None, None);
            }
        }
    }
}

fn window_belongs_to_flyout(flyout: HWND, candidate: HWND) -> bool {
    // SAFETY: GetAncestor accepts window handles and returns null for invalid ones.
    candidate.0 != 0
        && (candidate == flyout || unsafe { GetAncestor(candidate, GA_ROOTOWNER) == flyout })
}

unsafe extern "system" fn flyout_subclass(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
    _id: usize,
    _data: usize,
) -> LRESULT {
    match msg {
        WM_SHOW_FLYOUT => {
            *lock(&SHOWN_AT) = Some(Instant::now());
            let x = wparam.0 as i32;
            let y = lparam.0 as i32;
            position_flyout(hwnd, x, y);
            // SAFETY: Window visibility and timer changes run on the owning UI thread.
            unsafe {
                let _ = ShowWindow(hwnd, SW_SHOW);
                SetTimer(hwnd, DISMISS_TIMER, 75, None);
            }
            // SAFETY: hwnd is this window on its owning message thread.
            unsafe {
                let _ = SetForegroundWindow(hwnd);
            }
            LRESULT(0)
        }
        WM_HIDE_FLYOUT => {
            // SAFETY: This window and timer belong to the current UI thread.
            unsafe {
                let _ = ShowWindow(hwnd, SW_HIDE);
                let _ = KillTimer(hwnd, DISMISS_TIMER);
            }
            LRESULT(0)
        }
        WM_TIMER if wparam.0 == DISMISS_TIMER => {
            let shown_recently =
                lock(&SHOWN_AT).is_some_and(|shown| shown.elapsed() < FLYOUT_DISMISS_GRACE);
            // SAFETY: hwnd is the subclassed flyout. Key/foreground queries take no pointers.
            unsafe {
                if GetAsyncKeyState(VK_ESCAPE.0 as i32) < 0
                    || (!shown_recently && !window_belongs_to_flyout(hwnd, GetForegroundWindow()))
                {
                    let _ = ShowWindow(hwnd, SW_HIDE);
                    let _ = KillTimer(hwnd, DISMISS_TIMER);
                }
            }
            LRESULT(0)
        }
        WM_TIMER if wparam.0 == DISCOVERY_TIMER => {
            // SAFETY: The timer belongs to this subclassed window and is no longer needed.
            unsafe {
                let _ = KillTimer(hwnd, DISCOVERY_TIMER);
            }
            refresh_monitors_async(false);
            LRESULT(0)
        }
        WM_DISPLAYCHANGE => {
            // SAFETY: Setting the same window timer replaces its previous deadline.
            unsafe {
                SetTimer(hwnd, DISCOVERY_TIMER, 300, None);
            }
            // SAFETY: Forward this unmodified window message to the previous procedure.
            unsafe { DefSubclassProc(hwnd, msg, wparam, lparam) }
        }
        WM_POWERBROADCAST if wparam.0 == PBT_APMRESUMEAUTOMATIC => {
            // SAFETY: This timer is attached to the current window's message thread.
            unsafe {
                SetTimer(hwnd, DISCOVERY_TIMER, 1000, None);
            }
            LRESULT(1)
        }
        // SAFETY: Forward the unmodified message received by this installed subclass.
        _ => unsafe { DefSubclassProc(hwnd, msg, wparam, lparam) },
    }
}

fn request_show_flyout(anchor_x: i32, anchor_y: i32) {
    let hwnd = HWND(FLYOUT_HWND.load(Ordering::SeqCst));
    if hwnd.0 == 0 {
        return;
    }
    dismiss_tray_menu();
    // SAFETY: The posted coordinates are integers; no borrowed pointers cross threads.
    unsafe {
        let _ = PostMessageW(
            hwnd,
            WM_SHOW_FLYOUT,
            WPARAM(anchor_x as isize as usize),
            LPARAM(anchor_y as isize),
        );
    }
    if lock(shared_snapshot()).monitors.is_empty() {
        refresh_monitors_async(false);
    } else {
        load_selected_levels();
    }
}

fn request_hide_flyout() {
    let hwnd = HWND(FLYOUT_HWND.load(Ordering::SeqCst));
    if hwnd.0 == 0 {
        return;
    }
    // SAFETY: This message carries no pointers and runs visibility changes on the UI thread.
    unsafe {
        let _ = PostMessageW(hwnd, WM_HIDE_FLYOUT, None, None);
    }
}

fn toggle_flyout(anchor_x: i32, anchor_y: i32) {
    let hwnd = HWND(FLYOUT_HWND.load(Ordering::SeqCst));
    if hwnd.0 == 0 {
        return;
    }
    // SAFETY: IsWindowVisible accepts a window handle and has no output pointers.
    unsafe {
        if IsWindowVisible(hwnd).as_bool() {
            request_hide_flyout();
        } else {
            request_show_flyout(anchor_x, anchor_y);
        }
    }
}

fn show_flyout_default() {
    let mut cursor_point = POINT::default();
    // SAFETY: cursor_point is a live writable POINT during the synchronous call.
    unsafe {
        let _ = windows::Win32::UI::WindowsAndMessaging::GetCursorPos(&mut cursor_point);
    }
    request_show_flyout(cursor_point.x, cursor_point.y);
}

fn start_tray_event_threads() {
    let tray_events = TrayIconEvent::receiver();
    thread::spawn(move || {
        while let Ok(event) = tray_events.recv() {
            if let TrayIconEvent::Click {
                position,
                button: MouseButton::Left,
                button_state: MouseButtonState::Up,
                ..
            } = event
            {
                toggle_flyout(position.x.round() as i32, position.y.round() as i32);
            }
        }
    });

    let menu_events = tray_icon::menu::MenuEvent::receiver();
    thread::spawn(move || {
        while let Ok(event) = menu_events.recv() {
            match event.id().0.as_str() {
                "open" => show_flyout_default(),
                "refresh" => refresh_monitors_async(true),
                "exit" => std::process::exit(0),
                _ => {}
            }
        }
    });
}

fn create_tray() -> std::result::Result<TrayIcon, Box<dyn std::error::Error>> {
    let menu = Menu::new();
    menu.append(&MenuItem::with_id("open", "Open", true, None))?;
    menu.append(&MenuItem::with_id(
        "refresh",
        "Refresh Monitors",
        true,
        None,
    ))?;
    menu.append(&PredefinedMenuItem::separator())?;
    menu.append(&MenuItem::with_id("exit", "Exit", true, None))?;
    Ok(TrayIconBuilder::new()
        .with_menu(Box::new(menu))
        .with_menu_on_left_click(false)
        .with_tooltip("XCreen - Monitor Brightness Control")
        .with_icon(load_icon()?)
        .build()?)
}

fn init_dark_mode() {
    use windows::Win32::System::LibraryLoader::{
        GetProcAddress, LOAD_LIBRARY_SEARCH_SYSTEM32, LoadLibraryExW,
    };
    use windows::core::PCSTR;

    // SAFETY: Only the system copy of uxtheme is loaded and retained for process
    // lifetime. Ordinal 135 uses the PreferredAppMode ABI on supported Windows 11.
    unsafe {
        if let Ok(uxtheme) = LoadLibraryExW(
            windows::core::w!("uxtheme.dll"),
            None,
            LOAD_LIBRARY_SEARCH_SYSTEM32,
        ) {
            let ord_135 = PCSTR::from_raw(135 as *const u8);
            if let Some(set_preferred_app_mode) = GetProcAddress(uxtheme, ord_135) {
                // PreferredAppMode: AllowDark = 1
                let set_preferred_app_mode: unsafe extern "system" fn(i32) -> i32 =
                    std::mem::transmute(set_preferred_app_mode);
                set_preferred_app_mode(1);
            }
        }
    }
}

fn main() -> std::result::Result<(), Box<dyn std::error::Error>> {
    init_dark_mode();
    let loaded = AppConfig::load();
    let config = loaded.as_ref().cloned().unwrap_or_default();
    modules::logger::init_logger(&config.log_level)?;
    if let Err(err) = loaded {
        warn!("Config load error, using defaults: {err}");
    }
    if let Err(err) = set_autostart(config.autostart_enabled) {
        error!("Failed to apply autostart configuration: {err}");
    }

    let ambient_available = modules::sensor::has_light_sensor();
    let mut snapshot = UiSnapshot::empty(ambient_available);
    snapshot.autostart_enabled = config.autostart_enabled;
    snapshot.log_level = config.log_level.clone();
    CONFIG
        .set(Mutex::new(config))
        .map_err(|_| "config initialized twice")?;
    UI_SNAPSHOT
        .set(Mutex::new(snapshot))
        .map_err(|_| "UI snapshot initialized twice")?;
    start_monitor_worker()?;
    refresh_monitors_async(false);

    let _watcher = modules::watcher::start_config_watcher(|| {
        let mut current = lock(shared_config());
        let config = match AppConfig::load_existing() {
            Ok(config) => config,
            Err(err) => {
                warn!("Failed to reload config: {err}");
                return;
            }
        };
        let autostart_changed = current.autostart_enabled != config.autostart_enabled;
        let log_changed = !current.log_level.eq_ignore_ascii_case(&config.log_level);
        if !autostart_changed && !log_changed {
            return;
        }
        current.autostart_enabled = config.autostart_enabled;
        current.log_level = config.log_level;
        let autostart_enabled = current.autostart_enabled;
        let log_level = current.log_level.clone();
        if autostart_changed && let Err(err) = set_autostart(autostart_enabled) {
            error!("Failed to apply autostart configuration: {err}");
        }
        if log_changed {
            modules::logger::apply_log_level(&log_level);
            info!("Log level updated to {log_level}");
        }
        apply_settings_to_snapshot(autostart_enabled, &log_level);
        drop(current);
        publish_snapshot();
    })
    .map_err(|err| warn!("Config watcher unavailable: {err}"))
    .ok();

    App::new().run_custom(|_| {
        let host = ReactorHost::new_with_window_options(
            "XCreenFlyout",
            Some(WindowSize {
                width: FLYOUT_WIDTH_DIP,
                height: FLYOUT_HEIGHT_DIP,
            }),
            InnerConstraints {
                min_width: Some(FLYOUT_WIDTH_DIP),
                max_width: Some(FLYOUT_WIDTH_DIP),
                min_height: Some(FLYOUT_HEIGHT_DIP),
                max_height: Some(FLYOUT_HEIGHT_DIP),
            },
            Box::new(RenderFnComponent(app)),
            |_| {},
        )?;
        host.set_backdrop(Backdrop::Acrylic);

        let hwnd = current_thread_window().ok_or_else(windows_reactor::Error::from_thread)?;
        FLYOUT_HWND.store(hwnd.0, Ordering::SeqCst);
        configure_flyout_window(hwnd);
        // SAFETY: The callback has the required subclass ABI and is installed on the UI thread.
        unsafe {
            if !SetWindowSubclass(hwnd, Some(flyout_subclass), 0x5843_524e, 0).as_bool() {
                return Err(windows_reactor::Error::from_thread());
            }
        }

        // Hide window offscreen initially to prevent startup flash
        // SAFETY: hwnd is the initialized flyout on this thread.
        let dpi = unsafe { GetDpiForWindow(hwnd).max(96) };
        let width = (FLYOUT_WIDTH_DIP * dpi as f64 / 96.0).round() as i32;
        let height = (FLYOUT_HEIGHT_DIP * dpi as f64 / 96.0).round() as i32;
        // SAFETY: hwnd is the owned flyout, and all position arguments are integers.
        unsafe {
            let _ = SetWindowPos(
                hwnd,
                HWND_TOPMOST,
                -10000,
                -10000,
                width,
                height,
                SWP_NOACTIVATE,
            );
        }

        host.activate()?;
        // SAFETY: The activated flyout belongs to this UI thread.
        unsafe {
            let _ = ShowWindow(hwnd, SW_HIDE);
        }
        let tray = match create_tray() {
            Ok(tray) => tray,
            Err(err) => {
                error!("Unable to create tray icon: {}", err);
                return Err(windows_reactor::Error::from_thread());
            }
        };
        start_tray_event_threads();
        RUNTIME.with(|runtime| *runtime.borrow_mut() = Some((host, tray)));
        info!("XCreen started");
        Ok(())
    })?;
    Ok(())
}

struct RenderFnComponent(fn(&mut RenderCx) -> Element);

impl Component for RenderFnComponent {
    fn render(&self, _props: &(), cx: &mut RenderCx) -> Element {
        (self.0)(cx)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn monitor(id: &str) -> MonitorInfo {
        MonitorInfo {
            id: id.into(),
            name: id.into(),
            display_device: "DISPLAY1".into(),
            display_handle: 0,
            physical_index: 0,
            is_primary: false,
            levels: Some(MonitorLevels {
                brightness: Some(40),
                contrast: Some(50),
            }),
            level_error: None,
        }
    }

    #[test]
    fn discovery_preserves_edits_made_while_hardware_was_busy() {
        let monitors = [monitor("dell")];
        let mut snapshot = UiSnapshot::empty(false);
        sync_monitor_snapshot(&mut snapshot, &monitors, 0);
        snapshot.load_generation = 1;
        snapshot.levels = Some(MonitorLevels {
            brightness: Some(75),
            contrast: Some(50),
        });
        sync_monitor_snapshot(&mut snapshot, &monitors, 0);
        assert_eq!(
            snapshot.levels.and_then(|levels| levels.brightness),
            Some(75)
        );
        assert_eq!(snapshot.load_generation, 1);
    }

    #[test]
    fn disconnect_selects_a_remaining_monitor_and_clears_old_levels() {
        let mut snapshot = UiSnapshot::empty(false);
        sync_monitor_snapshot(&mut snapshot, &[monitor("dell"), monitor("lg")], 0);
        snapshot.selected = 1;
        let mut remaining = monitor("dell");
        remaining.levels = None;
        remaining.level_error = Some("Unavailable".into());
        sync_monitor_snapshot(&mut snapshot, &[remaining], 0);
        assert_eq!(
            snapshot.selected_monitor().map(|m| m.id.as_str()),
            Some("dell")
        );
        assert_eq!(snapshot.levels, None);
        assert_eq!(snapshot.error.as_deref(), Some("Unavailable"));
    }

    #[test]
    fn queued_edits_keep_both_channels_and_latest_value() {
        let queue = MonitorQueue::default();
        for (brightness, contrast, generation) in [
            (Some(20), None, 1),
            (None, Some(60), 2),
            (Some(80), None, 3),
        ] {
            queue.send(MonitorCommand::Apply {
                id: "dell".into(),
                levels: MonitorLevels {
                    brightness,
                    contrast,
                },
                generation,
            });
        }
        let mut pending = lock(&queue.commands);
        assert_eq!(pending.len(), 1);
        let Some((
            MonitorCommand::Apply {
                id,
                levels,
                generation,
            },
            _,
        )) = pending.pop_front()
        else {
            panic!("Expected a queued edit");
        };
        assert_eq!(id, "dell");
        assert_eq!(
            levels,
            MonitorLevels {
                brightness: Some(80),
                contrast: Some(60)
            }
        );
        assert_eq!(generation, 3);
    }

    #[test]
    fn refresh_requests_coalesce_without_losing_other_monitor_edits() {
        let queue = MonitorQueue::default();
        queue.send(MonitorCommand::Refresh);
        queue.send(MonitorCommand::Apply {
            id: "dell".into(),
            levels: MonitorLevels {
                brightness: Some(20),
                contrast: None,
            },
            generation: 1,
        });
        queue.send(MonitorCommand::Refresh);
        queue.send(MonitorCommand::Apply {
            id: "lg".into(),
            levels: MonitorLevels {
                brightness: Some(80),
                contrast: None,
            },
            generation: 2,
        });
        let pending = lock(&queue.commands);
        assert_eq!(pending.len(), 3);
        assert!(matches!(&pending[0].0, MonitorCommand::Apply { id, .. } if id == "dell"));
        assert!(matches!(&pending[1].0, MonitorCommand::Refresh));
        assert!(matches!(&pending[2].0, MonitorCommand::Apply { id, .. } if id == "lg"));
    }

    #[test]
    fn opening_selects_controllable_monitor_over_failing_primary() {
        let monitors = [
            UiMonitor {
                id: "laptop".into(),
                name: "Generic PnP Monitor".into(),
                is_primary: true,
                ready: false,
            },
            UiMonitor {
                id: "dell".into(),
                name: "Dell P2222H (HDMI)".into(),
                is_primary: false,
                ready: true,
            },
        ];
        assert_eq!(choose_selected_index(&monitors, None, false), 1);
        assert_eq!(choose_selected_index(&monitors, Some("dell"), true), 1);
        assert_eq!(choose_selected_index(&monitors, Some("laptop"), false), 1);
        assert_eq!(choose_selected_index(&monitors, Some("laptop"), true), 0);
    }

    #[test]
    fn flyout_anchors_to_bottom_right_with_padding() {
        let screen = RECT {
            left: 0,
            top: 0,
            right: 1920,
            bottom: 1080,
        };
        let (left, top) = calculate_flyout_position(screen, 360, 520, 8);
        assert_eq!((left, top), (1552, 552));

        let secondary = RECT {
            left: -1920,
            top: -200,
            right: 0,
            bottom: 880,
        };
        assert_eq!(
            calculate_flyout_position(secondary, 360, 520, 8),
            (-368, 352)
        );

        // Test clamping (e.g. if width/height is larger than screen size)
        let (left_clamp, top_clamp) = calculate_flyout_position(screen, 2000, 1200, 8);
        assert_eq!(left_clamp, 0);
        assert_eq!(top_clamp, 0);
    }
}
