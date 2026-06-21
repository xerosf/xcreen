#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod modules;

use ::image::ImageFormat;
use log::{error, info};
use modules::config::AppConfig;
use modules::monitor::{MonitorInfo, MonitorLevels};
use std::cell::RefCell;
use std::sync::atomic::{AtomicIsize, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::thread;
use std::time::Duration;
use tray_icon::{
    Icon, MouseButton, MouseButtonState, TrayIcon, TrayIconBuilder, TrayIconEvent,
    menu::{Menu, MenuItem, PredefinedMenuItem},
};
use windows::Win32::Foundation::{BOOL, HWND, LPARAM, POINT, RECT};
use windows::Win32::Graphics::Dwm::{DWMWINDOWATTRIBUTE, DwmSetWindowAttribute};
use windows::Win32::Graphics::Gdi::{
    GetMonitorInfoW, MONITOR_DEFAULTTONEAREST, MONITORINFO, MonitorFromPoint,
};
use windows::Win32::System::Threading::GetCurrentThreadId;
use windows::Win32::UI::HiDpi::{GetDpiForMonitor, GetDpiForWindow, MDT_EFFECTIVE_DPI};
use windows::Win32::UI::Input::KeyboardAndMouse::{GetAsyncKeyState, VK_ESCAPE, VK_LBUTTON};
use windows::Win32::UI::WindowsAndMessaging::{
    EnumThreadWindows, GWL_EXSTYLE, GWL_STYLE, GetForegroundWindow, GetWindowLongW, GetWindowRect,
    GetWindowThreadProcessId, HWND_TOPMOST, IsWindowVisible, PostMessageW, SW_HIDE, SW_SHOW,
    SWP_FRAMECHANGED, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE, SetForegroundWindow, SetWindowLongW,
    SetWindowPos, ShowWindow, WM_CANCELMODE, WS_CAPTION, WS_EX_APPWINDOW, WS_EX_TOOLWINDOW,
    WS_MAXIMIZEBOX, WS_MINIMIZEBOX, WS_SYSMENU, WS_THICKFRAME,
};
use windows_reactor::*;
use winreg::RegKey;
use winreg::enums::*;

const FLYOUT_WIDTH_DIP: f64 = 360.0;
const FLYOUT_HEIGHT_DIP: f64 = 280.0;

#[derive(Clone)]
struct AppState {
    monitors: Arc<Mutex<Vec<MonitorInfo>>>,
    config: Arc<Mutex<AppConfig>>,
    ambient_light_available: Arc<Mutex<bool>>,
}

impl AppState {
    fn new(config: AppConfig, ambient_light_available: bool) -> Self {
        Self {
            monitors: Arc::new(Mutex::new(Vec::new())),
            config: Arc::new(Mutex::new(config)),
            ambient_light_available: Arc::new(Mutex::new(ambient_light_available)),
        }
    }
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
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct UiSnapshot {
    monitors: Vec<UiMonitor>,
    selected: usize,
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

static UI_SNAPSHOT: OnceLock<Arc<Mutex<UiSnapshot>>> = OnceLock::new();
static UI_SETTER: OnceLock<Mutex<Option<AsyncSetState<UiSnapshot>>>> = OnceLock::new();
static APP_STATE: OnceLock<AppState> = OnceLock::new();
static FLYOUT_HWND: AtomicIsize = AtomicIsize::new(0);
static LOAD_GENERATION: AtomicU64 = AtomicU64::new(0);
static PENDING_COMMIT: Mutex<Option<(String, LevelKind, u32)>> = Mutex::new(None);

thread_local! {
    static RUNTIME: RefCell<Option<(ReactorHost, Option<TrayIcon>)>> = const { RefCell::new(None) };
}

fn shared_snapshot() -> &'static Arc<Mutex<UiSnapshot>> {
    UI_SNAPSHOT.get().expect("UI snapshot is initialized")
}

fn publish_snapshot() {
    let snapshot = shared_snapshot().lock().unwrap().clone();
    if let Some(setter) = UI_SETTER.get().and_then(|slot| slot.lock().ok()?.clone()) {
        setter.call(snapshot);
    }
}

fn refresh_snapshot_from_state(preserve_id: Option<&str>) {
    let state = APP_STATE.get().expect("app state is initialized");
    let monitors = state.monitors.lock().unwrap();
    let mut ui_monitors: Vec<UiMonitor> = monitors
        .iter()
        .map(|monitor| UiMonitor {
            id: monitor.id.clone(),
            name: monitor.name.clone(),
            is_primary: monitor.is_primary,
        })
        .collect();
    ui_monitors.sort_by_key(|monitor| !monitor.is_primary);

    let mut snapshot = shared_snapshot().lock().unwrap();
    let selected_id = preserve_id.map(str::to_owned).or_else(|| {
        snapshot
            .selected_monitor()
            .map(|monitor| monitor.id.clone())
    });

    let prev_selected_id = snapshot.selected_monitor().map(|m| m.id.clone());
    let id_changed =
        prev_selected_id.is_none() || selected_id.is_none() || prev_selected_id != selected_id;

    snapshot.monitors = ui_monitors;
    snapshot.selected = selected_id
        .as_deref()
        .and_then(|id| {
            snapshot
                .monitors
                .iter()
                .position(|monitor| monitor.id == id)
        })
        .unwrap_or(0);
    if id_changed {
        snapshot.levels = None;
    }
    snapshot.loading = false;
    snapshot.refreshing = false;
    snapshot.error = None;
    snapshot.ambient_available = *state.ambient_light_available.lock().unwrap();

    let config = state.config.lock().unwrap();
    snapshot.autostart_enabled = config.autostart_enabled;
    snapshot.log_level = config.log_level.clone();
}

fn refresh_monitors_sync(state: &AppState) -> std::result::Result<(), String> {
    let monitor_list = modules::monitor::get_monitor_list_sync()?;
    {
        let mut config = state.config.lock().unwrap();
        if config.merge_connected_monitors(&monitor_list) {
            config.save()?;
        }
    }
    *state.monitors.lock().unwrap() = monitor_list;
    Ok(())
}

fn refresh_monitors_async(explicit: bool) {
    {
        let mut snapshot = shared_snapshot().lock().unwrap();
        snapshot.loading = true;
        if explicit {
            snapshot.refreshing = true;
        }
        snapshot.error = None;
    }
    publish_snapshot();

    thread::spawn(|| {
        let state = APP_STATE.get().expect("app state is initialized");
        let selected = shared_snapshot()
            .lock()
            .unwrap()
            .selected_monitor()
            .map(|monitor| monitor.id.clone());
        match refresh_monitors_sync(state) {
            Ok(()) => {
                refresh_snapshot_from_state(selected.as_deref());
                publish_snapshot();
                load_selected_levels();
            }
            Err(err) => {
                let mut snapshot = shared_snapshot().lock().unwrap();
                snapshot.loading = false;
                snapshot.refreshing = false;
                snapshot.error = Some(err);
                drop(snapshot);
                publish_snapshot();
            }
        }
    });
}

fn load_selected_levels() {
    let monitor_id = {
        let mut snapshot = shared_snapshot().lock().unwrap();
        let Some(id) = snapshot
            .selected_monitor()
            .map(|monitor| monitor.id.clone())
        else {
            snapshot.loading = false;
            snapshot.levels = None;
            drop(snapshot);
            publish_snapshot();
            return;
        };
        snapshot.loading = true;
        snapshot.error = None;
        snapshot.load_generation = LOAD_GENERATION.fetch_add(1, Ordering::SeqCst) + 1;
        id
    };
    let generation = shared_snapshot().lock().unwrap().load_generation;
    publish_snapshot();

    thread::spawn(move || {
        let result = {
            let state = APP_STATE.get().expect("app state is initialized");
            let mut monitors = state.monitors.lock().unwrap();
            monitors
                .iter_mut()
                .find(|monitor| monitor.id == monitor_id)
                .ok_or_else(|| "The selected monitor is no longer connected".to_string())
                .and_then(modules::monitor::get_monitor_levels)
        };
        let mut snapshot = shared_snapshot().lock().unwrap();
        if snapshot.load_generation != generation
            || snapshot.selected_monitor().map(|m| m.id.as_str()) != Some(monitor_id.as_str())
        {
            return;
        }
        snapshot.loading = false;
        match result {
            Ok(levels) => snapshot.levels = Some(levels),
            Err(err) => {
                snapshot.levels = None;
                snapshot.error = Some(err);
            }
        }
        drop(snapshot);
        publish_snapshot();
    });
}

#[derive(Clone, Copy)]
enum LevelKind {
    Brightness,
    Contrast,
}

fn commit_level(monitor_id: String, kind: LevelKind, value: u32) {
    thread::spawn(move || {
        let result = {
            let state = APP_STATE.get().expect("app state is initialized");
            let mut monitors = state.monitors.lock().unwrap();
            let monitor = monitors
                .iter_mut()
                .find(|monitor| monitor.id == monitor_id)
                .ok_or_else(|| "The selected monitor is no longer connected".to_string());
            monitor.and_then(|monitor| match kind {
                LevelKind::Brightness => {
                    modules::monitor::set_monitor_brightness_only(monitor, value)
                }
                LevelKind::Contrast => modules::monitor::set_monitor_contrast_only(monitor, value),
            })
        };
        if let Err(err) = result {
            shared_snapshot().lock().unwrap().error = Some(err);
            publish_snapshot();
        } else {
            load_selected_levels();
        }
    });
}

fn apply_ambient(monitor_id: String) {
    thread::spawn(move || {
        let result = (|| {
            let state = APP_STATE.get().expect("app state is initialized");
            if !*state.ambient_light_available.lock().unwrap() {
                return Err("No ambient light sensor is available".to_string());
            }
            let lux = modules::sensor::get_light_sensor_lux()?;
            let (brightness, contrast) = match lux {
                x if x < 10.0 => (20, 40),
                x if x < 50.0 => (30, 45),
                x if x < 100.0 => (40, 55),
                x if x < 200.0 => (50, 65),
                x if x < 500.0 => (65, 75),
                x if x < 1000.0 => (80, 85),
                _ => (100, 95),
            };
            let mut monitors = state.monitors.lock().unwrap();
            let monitor = monitors
                .iter_mut()
                .find(|monitor| monitor.id == monitor_id)
                .ok_or_else(|| "The selected monitor is no longer connected".to_string())?;
            modules::monitor::set_monitor_settings_with_cache(monitor, brightness, contrast)?;
            Ok(())
        })();
        match result {
            Ok(()) => load_selected_levels(),
            Err(err) => {
                shared_snapshot().lock().unwrap().error = Some(err);
                publish_snapshot();
            }
        }
    });
}

fn navigate_monitor(delta: isize) {
    {
        let mut snapshot = shared_snapshot().lock().unwrap();
        let count = snapshot.monitors.len();
        if count == 0 {
            return;
        }
        snapshot.selected =
            (snapshot.selected as isize + delta).rem_euclid(count as isize) as usize;
        snapshot.levels = None;
        snapshot.error = None;
    }
    publish_snapshot();
    load_selected_levels();
}

fn update_local_level(kind: LevelKind, value: u32) {
    let mut snapshot = shared_snapshot().lock().unwrap();
    let monitor_id = snapshot.selected_monitor().map(|m| m.id.clone());
    let mut levels = snapshot.levels.unwrap_or(MonitorLevels {
        brightness: 0,
        contrast: 0,
    });
    match kind {
        LevelKind::Brightness => levels.brightness = value,
        LevelKind::Contrast => levels.contrast = value,
    }
    snapshot.levels = Some(levels);
    drop(snapshot);
    publish_snapshot();

    if let Some(id) = monitor_id {
        *PENDING_COMMIT.lock().unwrap() = Some((id, kind, value));
    }
}

fn update_autostart(enable: bool) {
    let state = APP_STATE.get().expect("app state is initialized");
    let mut config = state.config.lock().unwrap().clone();
    config.autostart_enabled = enable;
    if let Err(err) = config.save() {
        error!("Failed to save config: {err}");
        let mut snapshot = shared_snapshot().lock().unwrap();
        snapshot.error = Some(err);
        drop(snapshot);
        publish_snapshot();
        return;
    }
    if let Err(err) = set_autostart(enable) {
        error!("Failed to set registry autostart: {err}");
        let mut snapshot = shared_snapshot().lock().unwrap();
        snapshot.error = Some(err);
        drop(snapshot);
        publish_snapshot();
        return;
    }
    *state.config.lock().unwrap() = config;
    refresh_snapshot_from_state(None);
    publish_snapshot();
}

fn update_log_level(level: String) {
    let state = APP_STATE.get().expect("app state is initialized");
    let mut config = state.config.lock().unwrap().clone();
    config.log_level = level.clone();
    if let Err(err) = config.save() {
        error!("Failed to save config: {err}");
        let mut snapshot = shared_snapshot().lock().unwrap();
        snapshot.error = Some(err);
        drop(snapshot);
        publish_snapshot();
        return;
    }

    let level_filter = match level.to_lowercase().as_str() {
        "error" => log::LevelFilter::Error,
        "warn" => log::LevelFilter::Warn,
        "info" => log::LevelFilter::Info,
        "debug" => log::LevelFilter::Debug,
        _ => log::LevelFilter::Warn,
    };
    log::set_max_level(level_filter);

    *state.config.lock().unwrap() = config;
    refresh_snapshot_from_state(None);
    publish_snapshot();
}

fn app(cx: &mut RenderCx) -> Element {
    let initial = shared_snapshot().lock().unwrap().clone();
    let (snapshot, setter) = cx.use_async_state(initial);
    *UI_SETTER.get_or_init(|| Mutex::new(None)).lock().unwrap() = Some(setter);

    let main_inner: Element = if snapshot.monitors.is_empty() {
        vstack((
            subtitle("No compatible monitors"),
            body("Connect a DDC/CI monitor, then refresh."),
            button("Refresh").on_click(|| refresh_monitors_async(true)),
        ))
        .spacing(12.0)
        .into()
    } else {
        let monitor = snapshot.selected_monitor().unwrap().clone();
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
                    Slider::new(levels.brightness as f64)
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
                    Slider::new(levels.contrast as f64)
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
                    shared_snapshot().lock().unwrap().current_view = ActiveView::Main;
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
                    update_autostart(value);
                })
                .horizontal_alignment(HorizontalAlignment::Stretch),
            ComboBox::new(vec!["Error", "Warn", "Info", "Debug"])
                .header("Log level")
                .selected_index(match snapshot.log_level.to_lowercase().as_str() {
                    "error" => 0,
                    "warn" => 1,
                    "info" => 2,
                    "debug" => 3,
                    _ => 1,
                })
                .on_selection_changed(|index: i32| {
                    let level = match index {
                        0 => "error",
                        1 => "warn",
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
                shared_snapshot().lock().unwrap().current_view = ActiveView::Settings;
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
    } else if let Ok(key) = hkcu.open_subkey_with_flags(
        r"SOFTWARE\Microsoft\Windows\CurrentVersion\Run",
        KEY_SET_VALUE,
    ) {
        let _ = key.delete_value("XCreen");
    }
    Ok(())
}

#[allow(dead_code)]
fn open_config_file() -> std::result::Result<(), String> {
    use windows::Win32::UI::Shell::ShellExecuteW;
    use windows::Win32::UI::WindowsAndMessaging::SW_SHOW;
    use windows::core::PCWSTR;

    let config_path = AppConfig::get_config_path()?;
    let path_str = config_path.to_string_lossy();
    let mut path_u16: Vec<u16> = path_str.encode_utf16().collect();
    path_u16.push(0);
    let lpfile = PCWSTR::from_raw(path_u16.as_ptr());

    unsafe {
        let instance = ShellExecuteW(None, windows::core::w!("open"), lpfile, None, None, SW_SHOW);
        let status = instance.0 as isize;
        if status == 31 {
            // SE_ERR_NOASSOC
            let instance_openas = ShellExecuteW(
                None,
                windows::core::w!("openas"),
                lpfile,
                None,
                None,
                SW_SHOW,
            );
            let status_openas = instance_openas.0 as isize;
            if status_openas <= 32 {
                return Err(format!(
                    "Failed to open config file: ShellExecuteW(openas) returned {}",
                    status_openas
                ));
            }
        } else if status <= 32 {
            return Err(format!(
                "Failed to open config file: ShellExecuteW(open) returned {}",
                status
            ));
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
    let len =
        unsafe { windows::Win32::UI::WindowsAndMessaging::GetWindowTextW(hwnd, &mut title_buf) };
    let title = String::from_utf16_lossy(&title_buf[..len as usize]);

    let mut class_buf = [0u16; 512];
    let class_len =
        unsafe { windows::Win32::UI::WindowsAndMessaging::GetClassNameW(hwnd, &mut class_buf) };
    let class_name = String::from_utf16_lossy(&class_buf[..class_len as usize]);

    if title == "XCreenFlyout" {
        unsafe {
            *(lparam.0 as *mut HWND) = hwnd;
        }
        return BOOL(0); // Stop enumeration
    }

    if !class_name.contains("InputNonClientPointerSource")
        && !class_name.contains("DesktopWindowTreeSource")
    {
        unsafe {
            let backup_hwnd = lparam.0 as *mut HWND;
            if (*backup_hwnd).0 == 0 {
                *backup_hwnd = hwnd;
            }
        }
    }

    BOOL(1) // Continue enumeration
}

fn current_thread_window() -> Option<HWND> {
    let mut hwnd = HWND::default();
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
    unsafe {
        if GetDpiForMonitor(hmonitor, MDT_EFFECTIVE_DPI, &mut dpi_x, &mut dpi_y).is_ok() {
            dpi_x
        } else {
            96
        }
    }
}

fn position_flyout(hwnd: HWND, anchor_x: i32, anchor_y: i32) {
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

        let screen = info.rcMonitor;
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

fn toggle_flyout(anchor_x: i32, anchor_y: i32) {
    let hwnd = HWND(FLYOUT_HWND.load(Ordering::SeqCst));
    if hwnd.0 == 0 {
        return;
    }
    unsafe {
        if IsWindowVisible(hwnd).as_bool() {
            let _ = ShowWindow(hwnd, SW_HIDE);
        } else {
            dismiss_tray_menu();
            position_flyout(hwnd, anchor_x, anchor_y);
            refresh_monitors_async(false);
            let _ = ShowWindow(hwnd, SW_SHOW);
            let _ = SetForegroundWindow(hwnd);
        }
    }
}

fn show_flyout_default() {
    let hwnd = HWND(FLYOUT_HWND.load(Ordering::SeqCst));
    if hwnd.0 == 0 {
        return;
    }
    let mut cursor_point = POINT::default();
    unsafe {
        dismiss_tray_menu();
        let _ = windows::Win32::UI::WindowsAndMessaging::GetCursorPos(&mut cursor_point);
        position_flyout(hwnd, cursor_point.x, cursor_point.y);
        let _ = ShowWindow(hwnd, SW_SHOW);
        let _ = SetForegroundWindow(hwnd);
    }
    refresh_monitors_async(false);
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

    thread::spawn(|| {
        loop {
            thread::sleep(Duration::from_millis(75));
            let hwnd = HWND(FLYOUT_HWND.load(Ordering::SeqCst));

            let is_lbutton_down = unsafe { GetAsyncKeyState(VK_LBUTTON.0 as i32) < 0 };
            if !is_lbutton_down {
                let pending = {
                    let mut lock = PENDING_COMMIT.lock().unwrap();
                    lock.take()
                };
                if let Some((monitor_id, kind, value)) = pending {
                    commit_level(monitor_id, kind, value);
                }
            }

            if hwnd.0 == 0 {
                continue;
            }
            unsafe {
                if IsWindowVisible(hwnd).as_bool() {
                    let escaped = GetAsyncKeyState(VK_ESCAPE.0 as i32) < 0;
                    let foreground = GetForegroundWindow();
                    if escaped || foreground != hwnd {
                        let _ = ShowWindow(hwnd, SW_HIDE);
                    }
                }
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
    use windows::Win32::System::LibraryLoader::{GetProcAddress, LoadLibraryW};
    use windows::core::PCSTR;

    unsafe {
        if let Ok(uxtheme) = LoadLibraryW(windows::core::w!("uxtheme.dll")) {
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
    windows_reactor::bootstrap()?;
    let config = AppConfig::load().unwrap_or_else(|err| {
        eprintln!("Config load error, using defaults: {err}");
        AppConfig::default()
    });
    if let Err(err) = set_autostart(config.autostart_enabled) {
        eprintln!("Failed to apply autostart configuration: {err}");
    }
    if let Err(err) = modules::logger::init_logger(&config.log_level) {
        eprintln!("Logger init failed: {err}");
    }

    let ambient_available = modules::sensor::has_light_sensor();
    let state = AppState::new(config, ambient_available);
    if let Err(err) = refresh_monitors_sync(&state) {
        error!("Failed to initialize monitors: {err}");
    }
    APP_STATE
        .set(state)
        .map_err(|_| "app state initialized twice")?;
    UI_SNAPSHOT
        .set(Arc::new(Mutex::new(UiSnapshot::empty(ambient_available))))
        .map_err(|_| "UI snapshot initialized twice")?;
    refresh_snapshot_from_state(None);

    let watcher_state = APP_STATE.get().unwrap().clone();
    let _watcher = modules::watcher::start_config_watcher(move |mut config| {
        let monitors = watcher_state.monitors.lock().unwrap().clone();
        if config.merge_connected_monitors(&monitors) {
            let _ = config.save();
        }
        if let Err(err) = set_autostart(config.autostart_enabled) {
            error!("Failed to apply autostart configuration: {err}");
        }
        *watcher_state.config.lock().unwrap() = config;
        refresh_snapshot_from_state(None);
        publish_snapshot();
    });

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

        let hwnd = current_thread_window().expect("Unable to find WinUI window");
        FLYOUT_HWND.store(hwnd.0, Ordering::SeqCst);
        configure_flyout_window(hwnd);

        // Hide window offscreen initially to prevent startup flash
        let dpi = unsafe { GetDpiForWindow(hwnd).max(96) };
        let width = (FLYOUT_WIDTH_DIP * dpi as f64 / 96.0).round() as i32;
        let height = (FLYOUT_HEIGHT_DIP * dpi as f64 / 96.0).round() as i32;
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
        unsafe {
            let _ = ShowWindow(hwnd, SW_HIDE);
        }
        let tray = match create_tray() {
            Ok(t) => Some(t),
            Err(err) => {
                error!("Unable to create tray icon: {}", err);
                None
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

    #[test]
    fn monitor_navigation_wraps() {
        assert_eq!((0isize - 1).rem_euclid(3) as usize, 2);
        assert_eq!((2isize + 1).rem_euclid(3) as usize, 0);
    }

    #[test]
    fn primary_monitor_sorts_first() {
        let mut monitors = [
            UiMonitor {
                id: "secondary".into(),
                name: "B".into(),
                is_primary: false,
            },
            UiMonitor {
                id: "primary".into(),
                name: "A".into(),
                is_primary: true,
            },
        ];
        monitors.sort_by_key(|monitor| !monitor.is_primary);
        assert_eq!(monitors[0].id, "primary");
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
        assert_eq!(left, 1920 - 360 - 8);
        assert_eq!(top, 1080 - 520 - 8);

        // Test clamping (e.g. if width/height is larger than screen size)
        let (left_clamp, top_clamp) = calculate_flyout_position(screen, 2000, 1200, 8);
        assert_eq!(left_clamp, 0);
        assert_eq!(top_clamp, 0);
    }
}
