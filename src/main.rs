#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod modules;

use std::sync::Arc;
use std::sync::Mutex;
use std::thread;
use tray_icon::{
    menu::{Menu, MenuItem, Submenu, CheckMenuItem},
    TrayIconBuilder, Icon, MouseButton, MouseButtonState, TrayIconEvent
};
use winit::event_loop::{EventLoopBuilder, ControlFlow, EventLoopProxy};
use image::ImageFormat;
use winreg::enums::*;
use winreg::RegKey;
use log::{info, error, debug};

use modules::config::AppConfig;
use modules::monitor::MonitorInfo;
use modules::logger;
use modules::watcher;

// ─────────────────────────────────────────────────────────────────────────────
// Windows Dark Mode Support (Undocumented APIs)
// ─────────────────────────────────────────────────────────────────────────────

/// Enable dark mode for context menus on Windows 10 build 1809+
/// Uses undocumented uxtheme.dll ordinal functions
#[cfg(target_os = "windows")]
fn enable_dark_mode_for_app() {
    use windows::Win32::System::LibraryLoader::{GetProcAddress, LoadLibraryW};
    use windows::core::{PCWSTR, PCSTR};
    
    unsafe {
        // Load uxtheme.dll
        let module_name: Vec<u16> = "uxtheme.dll\0".encode_utf16().collect();
        let Ok(uxtheme) = LoadLibraryW(PCWSTR(module_name.as_ptr())) else {
            debug!("Failed to load uxtheme.dll");
            return;
        };
        
        // Try SetPreferredAppMode (Windows 10 1903+, ordinal 135)
        // Values: 0=Default, 1=AllowDark, 2=ForceDark, 3=ForceLight, 4=Max
        if let Some(set_preferred_app_mode) = GetProcAddress(uxtheme, PCSTR(135 as *const u8)) {
            let func: extern "system" fn(i32) -> i32 = std::mem::transmute(set_preferred_app_mode);
            func(1); // AllowDark
            debug!("SetPreferredAppMode(AllowDark) called");
        } else {
            // Fallback: Try AllowDarkModeForApp (Windows 10 1809, ordinal 132)
            if let Some(allow_dark) = GetProcAddress(uxtheme, PCSTR(132 as *const u8)) {
                let func: extern "system" fn(bool) -> bool = std::mem::transmute(allow_dark);
                func(true);
                debug!("AllowDarkModeForApp(true) called");
            }
        }
        
        // FlushMenuThemes (ordinal 136) - forces menu theme refresh
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
}

impl AppState {
    fn new(config: AppConfig) -> Self {
        Self {
            monitors: Mutex::new(Vec::new()),
            config: Mutex::new(config),
        }
    }
}

/// Events sent to the winit event loop
#[derive(Debug, Clone)]
enum AppEvent {
    SetAutostartChecked(bool),
}

/// Refresh monitor list from system
fn refresh_monitors(state: &AppState) -> Result<(), String> {
    let monitor_list = modules::monitor::get_monitor_list_sync()?;
    let mut monitors = state.monitors.lock().unwrap();
    *monitors = monitor_list;
    Ok(())
}

/// Apply brightness and contrast to all monitors
fn set_monitor_brightness(state: &AppState, brightness: u32, contrast: u32) -> Result<(), String> {
    let mut monitors = state.monitors.lock().unwrap().clone();
    
    if monitors.is_empty() {
        return Err("No monitors available".to_string());
    }
    
    let mut success_count = 0;
    let mut last_error = String::new();
    
    for monitor in monitors.iter_mut() {
        match modules::monitor::set_monitor_settings_with_cache(monitor, brightness, contrast) {
            Ok(_) => success_count += 1,
            Err(e) => last_error = e,
        }
    }
    
    // Update cached monitor state
    *state.monitors.lock().unwrap() = monitors;
    
    if success_count > 0 {
        Ok(())
    } else {
        Err(format!("Failed to update monitors: {}", last_error))
    }
}

/// Set brightness based on ambient light sensor reading
fn set_brightness_from_ambient_light(state: &AppState) -> Result<(), String> {
    let lux = modules::sensor::get_light_sensor_lux()?;
    debug!("Light sensor reading: {:.1} lux", lux);
    
    // Map lux to brightness/contrast percentages
    let (brightness, contrast) = match lux {
        x if x < 10.0 => (20, 40),
        x if x < 50.0 => (30, 45),
        x if x < 100.0 => (40, 55),
        x if x < 200.0 => (50, 65),
        x if x < 500.0 => (65, 75),
        x if x < 1000.0 => (80, 85),
        _ => (100, 95),
    };
    
    set_monitor_brightness(state, brightness, contrast)?;
    info!("Brightness set from ambient light: {}% brightness, {}% contrast ({:.1} lux)", brightness, contrast, lux);
    Ok(())
}

/// Apply a named brightness profile
fn apply_brightness_profile(state: &AppState, profile_name: &str) -> Result<(), String> {
    let (brightness, contrast) = {
        let config = state.config.lock().unwrap();
        config.brightness_profiles.iter()
            .find(|p| p.name.eq_ignore_ascii_case(profile_name))
            .map(|p| (p.brightness, p.contrast))
            .ok_or_else(|| format!("Profile '{}' not found", profile_name))?
    };
    
    set_monitor_brightness(state, brightness, contrast)?;
    info!("Applied profile '{}': {}% brightness, {}% contrast", profile_name, brightness, contrast);
    Ok(())
}

// ─────────────────────────────────────────────────────────────────────────────
// Autostart (Windows Registry)
// ─────────────────────────────────────────────────────────────────────────────

fn check_autostart_enabled() -> bool {
    let hkcu = RegKey::predef(HKEY_CURRENT_USER);
    hkcu.open_subkey_with_flags(r"SOFTWARE\Microsoft\Windows\CurrentVersion\Run", KEY_QUERY_VALUE)
        .and_then(|key| key.get_value::<String, _>("XCreen"))
        .is_ok()
}

fn set_autostart(enable: bool) -> Result<(), String> {
    let hkcu = RegKey::predef(HKEY_CURRENT_USER);
    
    if enable {
        let (key, _) = hkcu.create_subkey(r"SOFTWARE\Microsoft\Windows\CurrentVersion\Run")
            .map_err(|e| format!("Failed to open registry: {}", e))?;
        
        let exe_path = std::env::current_exe()
            .map_err(|e| format!("Failed to get exe path: {}", e))?;
        
        key.set_value("XCreen", &format!("\"{}\"", exe_path.display()))
            .map_err(|e| format!("Failed to set registry value: {}", e))?;
    } else if let Ok(key) = hkcu.open_subkey_with_flags(r"SOFTWARE\Microsoft\Windows\CurrentVersion\Run", KEY_SET_VALUE) {
        let _ = key.delete_value("XCreen"); // Ignore if doesn't exist
    }
    
    Ok(())
}

fn toggle_autostart(state: &AppState) -> Result<bool, String> {
    let current = check_autostart_enabled();
    let target = !current;
    
    set_autostart(target)?;
    
    // Update config to match
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

fn create_tray_menu(state: &AppState) -> Result<(Menu, CheckMenuItem), Box<dyn std::error::Error>> {
    let autostart_enabled = check_autostart_enabled();
    let profiles = state.config.lock().unwrap().brightness_profiles.clone();

    let menu = Menu::new();
    
    // Ambient light option
    menu.append(&MenuItem::with_id("ambient_light", "Set from Ambient Light", true, None))?;
    
    // Autostart checkbox
    let autostart_item = CheckMenuItem::with_id("autostart", "Autostart", true, autostart_enabled, None);
    menu.append(&autostart_item)?;
    
    // Brightness profiles submenu
    let profiles_submenu = Submenu::with_id("brightness_submenu", "Set Brightness", true);
    for profile in &profiles {
        profiles_submenu.append(&MenuItem::with_id(
            profile.name.to_lowercase(),
            &profile.name,
            true,
            None
        ))?;
    }
    menu.append(&profiles_submenu)?;
    
    // Refresh and exit
    menu.append(&MenuItem::with_id("refresh", "Refresh Monitors", true, None))?;
    menu.append(&MenuItem::with_id("quit", "Exit", true, None))?;

    Ok((menu, autostart_item))
}

fn handle_menu_event(state: &AppState, proxy: &EventLoopProxy<AppEvent>, menu_id: &str) {
    match menu_id {
        "ambient_light" => {
            if let Err(e) = set_brightness_from_ambient_light(state) {
                error!("Ambient light failed: {}", e);
            }
        }
        "autostart" => {
            match toggle_autostart(state) {
                Ok(enabled) => {
                    let _ = proxy.send_event(AppEvent::SetAutostartChecked(enabled));
                }
                Err(e) => error!("Toggle autostart failed: {}", e),
            }
        }
        "refresh" => {
            if let Err(e) = refresh_monitors(state) {
                error!("Refresh monitors failed: {}", e);
            } else {
                info!("Monitors refreshed");
            }
        }
        "quit" => {
            info!("Exiting XCreen");
            std::process::exit(0);
        }
        "brightness_submenu" => {} // Ignore submenu container clicks
        profile_name => {
            // Check if valid profile
            let is_profile = state.config.lock().unwrap()
                .brightness_profiles.iter()
                .any(|p| p.name.eq_ignore_ascii_case(profile_name));
            
            if is_profile {
                if let Err(e) = apply_brightness_profile(state, profile_name) {
                    error!("Apply profile '{}' failed: {}", profile_name, e);
                }
            } else {
                debug!("Unknown menu item: {}", profile_name);
            }
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Main
// ─────────────────────────────────────────────────────────────────────────────

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Enable dark mode for context menus (must be called early, before creating menus)
    enable_dark_mode_for_app();
    
    // Load configuration
    let config = AppConfig::load().unwrap_or_else(|e| {
        eprintln!("Config load error, using defaults: {}", e);
        AppConfig::default()
    });

    // Initialize logger
    if let Err(e) = logger::init_logger(&config.log_level) {
        eprintln!("Logger init failed: {}", e);
    }

    let state = Arc::new(AppState::new(config));

    // Initialize monitors
    if let Err(e) = refresh_monitors(&state) {
        error!("Failed to initialize monitors: {}", e);
    } else {
        let count = state.monitors.lock().unwrap().len();
        info!("Initialized {} monitor(s)", count);
    }

    // Sync config autostart with registry (registry is source of truth)
    let registry_autostart = check_autostart_enabled();
    {
        let mut config = state.config.lock().unwrap();
        if config.autostart_enabled != registry_autostart {
            config.autostart_enabled = registry_autostart;
            let _ = config.save();
        }
    }

    // Create event loop
    let event_loop = EventLoopBuilder::<AppEvent>::with_user_event().build()?;
    let proxy = event_loop.create_proxy();
    
    // Create tray icon
    let icon = load_icon()?;
    let (menu, autostart_item) = create_tray_menu(&state)?;
    
    let _tray = TrayIconBuilder::new()
        .with_menu(Box::new(menu))
        .with_tooltip("XCreen - Monitor Brightness Control")
        .with_icon(icon)
        .build()?;
    
    info!("XCreen started");

    // Config file watcher
    let proxy_for_watcher = proxy.clone();
    let state_for_watcher = Arc::clone(&state);
    let _config_watcher = watcher::start_config_watcher(move |new_config| {
        // Update state config
        if let Ok(mut config) = state_for_watcher.config.lock() {
            *config = new_config.clone();
        }
        
        // Sync autostart checkbox
        let _ = proxy_for_watcher.send_event(AppEvent::SetAutostartChecked(new_config.autostart_enabled));
        
        // Clear monitor cache to force re-apply on next action
        if let Ok(mut monitors) = state_for_watcher.monitors.lock() {
            for m in monitors.iter_mut() {
                m.cached_brightness = None;
                m.cached_contrast = None;
            }
        }
    });

    // Menu event handler thread
    let menu_channel = tray_icon::menu::MenuEvent::receiver();
    let state_for_menu = Arc::clone(&state);
    let proxy_for_menu = proxy.clone();
    
    thread::spawn(move || {
        while let Ok(event) = menu_channel.recv() {
            handle_menu_event(&state_for_menu, &proxy_for_menu, &event.id().0);
        }
    });

    // Tray click handler (optional - just for logging)
    let tray_receiver = TrayIconEvent::receiver();
    thread::spawn(move || {
        while let Ok(event) = tray_receiver.recv() {
            if let TrayIconEvent::Click { button: MouseButton::Left, button_state: MouseButtonState::Down, .. } = event {
                debug!("Tray icon clicked");
            }
        }
    });

    // Run event loop
    event_loop.run(move |event, elwt| {
        elwt.set_control_flow(ControlFlow::Wait);
        
        if let winit::event::Event::UserEvent(AppEvent::SetAutostartChecked(checked)) = event {
            autostart_item.set_checked(checked);
        }
    })?;

    Ok(())
}
