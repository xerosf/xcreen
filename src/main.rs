#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod modules;

use image::ImageFormat;
use log::{debug, error, info};
use std::process::Command;
use std::sync::{Arc, Mutex};
use std::thread;
use tray_icon::{
    Icon, MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent,
    menu::{CheckMenuItem, Menu, MenuItem, PredefinedMenuItem, Submenu},
};
use winit::event_loop::{ControlFlow, EventLoopBuilder};
use winreg::RegKey;
use winreg::enums::*;

use modules::config::AppConfig;
use modules::logger;
use modules::monitor::MonitorInfo;
use modules::watcher;

// ─────────────────────────────────────────────────────────────────────────────
// Windows Dark Mode Support (Undocumented APIs)
// ─────────────────────────────────────────────────────────────────────────────

/// Enable dark mode for context menus on Windows 10 build 1809+
/// Uses undocumented uxtheme.dll ordinal functions
#[cfg(target_os = "windows")]
fn enable_dark_mode_for_app() {
    use windows::Win32::System::LibraryLoader::{GetProcAddress, LoadLibraryW};
    use windows::core::{PCSTR, PCWSTR};

    unsafe {
        let module_name: Vec<u16> = "uxtheme.dll\0".encode_utf16().collect();
        let Ok(uxtheme) = LoadLibraryW(PCWSTR(module_name.as_ptr())) else {
            debug!("Failed to load uxtheme.dll");
            return;
        };

        if let Some(set_preferred_app_mode) = GetProcAddress(uxtheme, PCSTR(135 as *const u8)) {
            let func: extern "system" fn(i32) -> i32 = std::mem::transmute(set_preferred_app_mode);
            func(1);
            debug!("SetPreferredAppMode(AllowDark) called");
        } else if let Some(allow_dark) = GetProcAddress(uxtheme, PCSTR(132 as *const u8)) {
            let func: extern "system" fn(bool) -> bool = std::mem::transmute(allow_dark);
            func(true);
            debug!("AllowDarkModeForApp(true) called");
        }

        if let Some(flush_menu_themes) = GetProcAddress(uxtheme, PCSTR(136 as *const u8)) {
            let func: extern "system" fn() = std::mem::transmute(flush_menu_themes);
            func();
            debug!("FlushMenuThemes called");
        }
    }
}

#[cfg(not(target_os = "windows"))]
fn enable_dark_mode_for_app() {}

/// Shared application state
struct AppState {
    monitors: Mutex<Vec<MonitorInfo>>,
    config: Mutex<AppConfig>,
    ambient_light_available: Mutex<bool>,
}

impl AppState {
    fn new(config: AppConfig, ambient_light_available: bool) -> Self {
        Self {
            monitors: Mutex::new(Vec::new()),
            config: Mutex::new(config),
            ambient_light_available: Mutex::new(ambient_light_available),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ProfileMenuSelection {
    monitor_id: String,
    profile_index: usize,
}

impl ProfileMenuSelection {
    fn menu_id(monitor_id: &str, profile_index: usize) -> String {
        format!("profile:{}:{}", monitor_id, profile_index)
    }

    fn parse(menu_id: &str) -> Option<Self> {
        let rest = menu_id.strip_prefix("profile:")?;
        let (monitor_id, profile_index) = rest.rsplit_once(':')?;
        Some(Self {
            monitor_id: monitor_id.to_string(),
            profile_index: profile_index.parse().ok()?,
        })
    }
}

#[derive(Clone)]
struct TrayMenuHandles {
    autostart_item: CheckMenuItem,
    ambient_light_item: Submenu,
}

/// Events sent to the winit event loop
#[derive(Debug, Clone)]
enum AppEvent {
    Menu(String),
    SetAutostartChecked(bool),
    SetAmbientLightEnabled(bool),
    RebuildMenu,
}

/// Refresh monitor list from system and ensure connected monitors exist in config
fn refresh_monitors(state: &AppState) -> Result<(), String> {
    let monitor_list = modules::monitor::get_monitor_list_sync()?;

    {
        let mut config = state.config.lock().unwrap();
        if config.merge_connected_monitors(&monitor_list) {
            config.save()?;
        }
    }

    let mut monitors = state.monitors.lock().unwrap();
    *monitors = monitor_list;
    Ok(())
}

/// Apply brightness and contrast to one monitor
fn set_monitor_brightness(
    state: &AppState,
    monitor_id: &str,
    brightness: u32,
    contrast: u32,
) -> Result<(), String> {
    let mut monitors = state.monitors.lock().unwrap();
    let monitor = monitors
        .iter_mut()
        .find(|monitor| monitor.id == monitor_id)
        .ok_or_else(|| format!("Monitor '{}' is not connected", monitor_id))?;

    modules::monitor::set_monitor_settings_with_cache(monitor, brightness, contrast)?;
    Ok(())
}

/// Set brightness based on ambient light sensor reading
fn set_brightness_from_ambient_light(state: &AppState, monitor_id: &str) -> Result<(), String> {
    if !modules::sensor::has_light_sensor() {
        if let Ok(mut available) = state.ambient_light_available.lock() {
            *available = false;
        }
        return Err("No ambient light sensor available".to_string());
    }

    let lux = modules::sensor::get_light_sensor_lux()?;
    debug!("Light sensor reading: {:.1} lux", lux);

    let (brightness, contrast) = match lux {
        x if x < 10.0 => (20, 40),
        x if x < 50.0 => (30, 45),
        x if x < 100.0 => (40, 55),
        x if x < 200.0 => (50, 65),
        x if x < 500.0 => (65, 75),
        x if x < 1000.0 => (80, 85),
        _ => (100, 95),
    };

    set_monitor_brightness(state, monitor_id, brightness, contrast)?;
    info!(
        "Brightness set from ambient light for monitor '{}': {}% brightness, {}% contrast ({:.1} lux)",
        monitor_id, brightness, contrast, lux
    );
    Ok(())
}

/// Apply a named brightness profile to one monitor
fn apply_brightness_profile(
    state: &AppState,
    monitor_id: &str,
    profile_index: usize,
) -> Result<(), String> {
    let (profile_name, brightness, contrast) = {
        let config = state.config.lock().unwrap();
        let profiles = config
            .profiles_for_monitor(monitor_id)
            .ok_or_else(|| format!("Monitor '{}' is missing from config", monitor_id))?;
        let profile = profiles
            .get(profile_index)
            .ok_or_else(|| format!("Profile index {} not found", profile_index))?;
        (profile.name.clone(), profile.brightness, profile.contrast)
    };

    set_monitor_brightness(state, monitor_id, brightness, contrast)?;
    info!(
        "Applied profile '{}' to monitor '{}': {}% brightness, {}% contrast",
        profile_name, monitor_id, brightness, contrast
    );
    Ok(())
}

// ─────────────────────────────────────────────────────────────────────────────
// Autostart (Windows Registry)
// ─────────────────────────────────────────────────────────────────────────────

fn check_autostart_enabled() -> bool {
    let hkcu = RegKey::predef(HKEY_CURRENT_USER);
    hkcu.open_subkey_with_flags(
        r"SOFTWARE\Microsoft\Windows\CurrentVersion\Run",
        KEY_QUERY_VALUE,
    )
    .and_then(|key| key.get_value::<String, _>("XCreen"))
    .is_ok()
}

fn set_autostart(enable: bool) -> Result<(), String> {
    let hkcu = RegKey::predef(HKEY_CURRENT_USER);

    if enable {
        let (key, _) = hkcu
            .create_subkey(r"SOFTWARE\Microsoft\Windows\CurrentVersion\Run")
            .map_err(|e| format!("Failed to open registry: {}", e))?;

        let exe_path =
            std::env::current_exe().map_err(|e| format!("Failed to get exe path: {}", e))?;

        key.set_value("XCreen", &format!("\"{}\"", exe_path.display()))
            .map_err(|e| format!("Failed to set registry value: {}", e))?;
    } else if let Ok(key) = hkcu.open_subkey_with_flags(
        r"SOFTWARE\Microsoft\Windows\CurrentVersion\Run",
        KEY_SET_VALUE,
    ) {
        let _ = key.delete_value("XCreen");
    }

    Ok(())
}

fn toggle_autostart(state: &AppState) -> Result<bool, String> {
    let current = check_autostart_enabled();
    let target = !current;

    set_autostart(target)?;

    {
        let mut config = state.config.lock().unwrap();
        config.autostart_enabled = target;
        config.save()?;
    }

    info!("Autostart {}", if target { "enabled" } else { "disabled" });
    Ok(target)
}

// ─────────────────────────────────────────────────────────────────────────────
// Tray Menu
// ─────────────────────────────────────────────────────────────────────────────

fn load_icon() -> Result<Icon, Box<dyn std::error::Error>> {
    let icon_data = include_bytes!("icons/32x32.png");
    let image = image::load_from_memory_with_format(icon_data, ImageFormat::Png)?;
    let rgba = image.to_rgba8();
    let (width, height) = rgba.dimensions();
    Ok(Icon::from_rgba(rgba.into_raw(), width, height)?)
}

fn create_tray_menu(
    state: &AppState,
) -> Result<(Menu, TrayMenuHandles), Box<dyn std::error::Error>> {
    let autostart_enabled = check_autostart_enabled();
    let ambient_light_available = *state.ambient_light_available.lock().unwrap();
    let monitors = state.monitors.lock().unwrap().clone();
    let config = state.config.lock().unwrap().clone();

    let menu = Menu::new();

    let ambient_light_item = Submenu::with_id(
        "ambient_light",
        "Set from Ambient Light",
        ambient_light_available && !monitors.is_empty(),
    );
    for monitor in &monitors {
        let label = if monitor.is_primary {
            format!("{} (Primary)", monitor.name)
        } else {
            monitor.name.clone()
        };
        ambient_light_item.append(&MenuItem::with_id(
            format!("ambient:{}", monitor.id),
            label,
            ambient_light_available,
            None,
        ))?;
    }
    menu.append(&ambient_light_item)?;
    menu.append(&MenuItem::with_id(
        "open_config_folder",
        "Open Config Folder",
        true,
        None,
    ))?;

    let autostart_item =
        CheckMenuItem::with_id("autostart", "Autostart", true, autostart_enabled, None);
    menu.append(&autostart_item)?;
    menu.append(&MenuItem::with_id(
        "refresh",
        "Refresh Monitors",
        true,
        None,
    ))?;
    menu.append(&PredefinedMenuItem::separator())?;

    if monitors.is_empty() {
        menu.append(&MenuItem::with_id(
            "no_monitors",
            "No DDC/CI monitors detected",
            false,
            None,
        ))?;
    } else {
        for monitor in &monitors {
            let label = if monitor.is_primary {
                format!("{} (Primary)", monitor.name)
            } else {
                monitor.name.clone()
            };
            let monitor_submenu = Submenu::with_id(format!("monitor:{}", monitor.id), label, true);

            if let Some(profiles) = config.profiles_for_monitor(&monitor.id) {
                for (profile_index, profile) in profiles.iter().enumerate() {
                    monitor_submenu.append(&MenuItem::with_id(
                        ProfileMenuSelection::menu_id(&monitor.id, profile_index),
                        &profile.name,
                        true,
                        None,
                    ))?;
                }
            }

            menu.append(&monitor_submenu)?;
        }
    }

    menu.append(&PredefinedMenuItem::separator())?;
    menu.append(&MenuItem::with_id("quit", "Exit", true, None))?;

    Ok((
        menu,
        TrayMenuHandles {
            autostart_item,
            ambient_light_item,
        },
    ))
}

fn open_config_folder() -> Result<(), String> {
    let config_path = AppConfig::get_config_path()?;
    let config_dir = config_path
        .parent()
        .ok_or("Failed to get config directory")?;

    Command::new("explorer")
        .arg(config_dir)
        .spawn()
        .map_err(|e| format!("Failed to open config folder: {}", e))?;

    Ok(())
}

fn handle_menu_event(state: &AppState, menu_id: &str) -> Option<AppEvent> {
    match menu_id {
        "ambient_light" => {}
        "autostart" => match toggle_autostart(state) {
            Ok(enabled) => return Some(AppEvent::SetAutostartChecked(enabled)),
            Err(e) => error!("Toggle autostart failed: {}", e),
        },
        "open_config_folder" => {
            if let Err(e) = open_config_folder() {
                error!("Open config folder failed: {}", e);
            }
        }
        "refresh" => match refresh_monitors(state) {
            Ok(()) => {
                info!("Monitors refreshed");
                return Some(AppEvent::RebuildMenu);
            }
            Err(e) => error!("Refresh monitors failed: {}", e),
        },
        "quit" => {
            info!("Exiting XCreen");
            std::process::exit(0);
        }
        "no_monitors" => {}
        id if id.starts_with("monitor:") => {}
        id if id.starts_with("ambient:") => {
            let monitor_id = id.trim_start_matches("ambient:");
            if let Err(e) = set_brightness_from_ambient_light(state, monitor_id) {
                error!("Ambient light failed: {}", e);
                return Some(AppEvent::SetAmbientLightEnabled(false));
            }
        }
        profile_id => {
            if let Some(selection) = ProfileMenuSelection::parse(profile_id) {
                if let Err(e) =
                    apply_brightness_profile(state, &selection.monitor_id, selection.profile_index)
                {
                    error!("Apply profile failed: {}", e);
                }
            } else {
                debug!("Unknown menu item: {}", menu_id);
            }
        }
    }

    None
}

// ─────────────────────────────────────────────────────────────────────────────
// Main
// ─────────────────────────────────────────────────────────────────────────────

fn main() -> Result<(), Box<dyn std::error::Error>> {
    enable_dark_mode_for_app();

    let config = AppConfig::load().unwrap_or_else(|e| {
        eprintln!("Config load error, using defaults: {}", e);
        AppConfig::default()
    });

    if let Err(e) = logger::init_logger(&config.log_level) {
        eprintln!("Logger init failed: {}", e);
    }

    let ambient_light_available = modules::sensor::has_light_sensor();
    let state = Arc::new(AppState::new(config, ambient_light_available));

    if let Err(e) = refresh_monitors(&state) {
        error!("Failed to initialize monitors: {}", e);
    } else {
        let count = state.monitors.lock().unwrap().len();
        info!("Initialized {} physical monitor(s)", count);
    }

    let registry_autostart = check_autostart_enabled();
    {
        let mut config = state.config.lock().unwrap();
        if config.autostart_enabled != registry_autostart {
            config.autostart_enabled = registry_autostart;
            let _ = config.save();
        }
    }

    let event_loop = EventLoopBuilder::<AppEvent>::with_user_event().build()?;
    let proxy = event_loop.create_proxy();

    let icon = load_icon()?;
    let (menu, mut tray_handles) = create_tray_menu(&state)?;

    let tray = TrayIconBuilder::new()
        .with_menu(Box::new(menu))
        .with_tooltip("XCreen - Monitor Brightness Control")
        .with_icon(icon)
        .build()?;

    info!("XCreen started");

    let proxy_for_watcher = proxy.clone();
    let state_for_watcher = Arc::clone(&state);
    let _config_watcher = watcher::start_config_watcher(move |mut new_config| {
        let monitors = state_for_watcher
            .monitors
            .lock()
            .map(|monitors| monitors.clone())
            .unwrap_or_default();

        if new_config.merge_connected_monitors(&monitors) {
            let _ = new_config.save();
        }

        new_config.autostart_enabled = check_autostart_enabled();

        if let Ok(mut config) = state_for_watcher.config.lock() {
            *config = new_config;
        }

        if let Ok(mut monitors) = state_for_watcher.monitors.lock() {
            for monitor in monitors.iter_mut() {
                monitor.cached_brightness = None;
                monitor.cached_contrast = None;
            }
        }

        let _ =
            proxy_for_watcher.send_event(AppEvent::SetAutostartChecked(check_autostart_enabled()));
        let _ = proxy_for_watcher.send_event(AppEvent::RebuildMenu);
    });

    let menu_channel = tray_icon::menu::MenuEvent::receiver();
    let proxy_for_menu = proxy.clone();

    thread::spawn(move || {
        while let Ok(event) = menu_channel.recv() {
            let _ = proxy_for_menu.send_event(AppEvent::Menu(event.id().0.clone()));
        }
    });

    let tray_receiver = TrayIconEvent::receiver();
    thread::spawn(move || {
        while let Ok(event) = tray_receiver.recv() {
            if let TrayIconEvent::Click {
                button: MouseButton::Left,
                button_state: MouseButtonState::Down,
                ..
            } = event
            {
                debug!("Tray icon clicked");
            }
        }
    });

    event_loop.run(move |event, elwt| {
        elwt.set_control_flow(ControlFlow::Wait);

        if let winit::event::Event::UserEvent(app_event) = event {
            match app_event {
                AppEvent::Menu(menu_id) => {
                    if let Some(next_event) = handle_menu_event(&state, &menu_id) {
                        let _ = proxy.send_event(next_event);
                    }
                }
                AppEvent::SetAutostartChecked(checked) => {
                    tray_handles.autostart_item.set_checked(checked);
                }
                AppEvent::SetAmbientLightEnabled(enabled) => {
                    if let Ok(mut available) = state.ambient_light_available.lock() {
                        *available = enabled;
                    }
                    tray_handles.ambient_light_item.set_enabled(enabled);
                    let _ = proxy.send_event(AppEvent::RebuildMenu);
                }
                AppEvent::RebuildMenu => match create_tray_menu(&state) {
                    Ok((menu, handles)) => {
                        tray.set_menu(Some(Box::new(menu)));
                        tray_handles = handles;
                    }
                    Err(e) => error!("Failed to rebuild tray menu: {}", e),
                },
            }
        }
    })?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::ProfileMenuSelection;

    #[test]
    fn parses_profile_menu_ids_with_reserved_profile_names() {
        let id = ProfileMenuSelection::menu_id("monitor-refresh-quit", 2);
        let parsed = ProfileMenuSelection::parse(&id).unwrap();

        assert_eq!(parsed.monitor_id, "monitor-refresh-quit");
        assert_eq!(parsed.profile_index, 2);
    }

    #[test]
    fn rejects_non_profile_menu_ids() {
        assert!(ProfileMenuSelection::parse("refresh").is_none());
        assert!(ProfileMenuSelection::parse("ambient_light").is_none());
        assert!(ProfileMenuSelection::parse("profile:missing-index").is_none());
    }
}
