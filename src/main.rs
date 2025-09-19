#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod modules;

use std::sync::Arc;
use std::sync::Mutex;
use std::time::{Duration, Instant};
use std::thread;
use tray_icon::{
    menu::{Menu, MenuItem, Submenu, CheckMenuItem},
    TrayIconBuilder, Icon, TrayIconEvent, MouseButton, MouseButtonState
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

#[derive(Clone)]
struct AppState {
    autostart_enabled: Arc<Mutex<bool>>,
    last_brightness: Arc<Mutex<u32>>,
    monitors: Arc<Mutex<Vec<MonitorInfo>>>,
    config: Arc<Mutex<AppConfig>>,
    should_exit: Arc<Mutex<bool>>,
    runtime: Arc<tokio::runtime::Runtime>,
    last_hardware_write: Arc<Mutex<std::time::Instant>>,
    hardware_write_count: Arc<Mutex<u32>>,
}


// Custom application events sent to the winit event loop
#[derive(Debug, Clone)]
enum AppEvent {
    SetAutostartChecked(bool),
}



impl AppState {
    fn new(config: AppConfig) -> Result<Self, String> {
        let runtime = tokio::runtime::Runtime::new()
            .map_err(|e| format!("Failed to create tokio runtime: {}", e))?;
        
        Ok(Self {
            autostart_enabled: Arc::new(Mutex::new(config.autostart_enabled)),
            last_brightness: Arc::new(Mutex::new(config.last_brightness)),
            monitors: Arc::new(Mutex::new(Vec::new())),
            config: Arc::new(Mutex::new(config)),
            should_exit: Arc::new(Mutex::new(false)),
            runtime: Arc::new(runtime),
            last_hardware_write: Arc::new(Mutex::new(std::time::Instant::now())),
            hardware_write_count: Arc::new(Mutex::new(0)),
        })
    }
}

fn calculate_brightness_from_lux(lux: f64) -> u32 {
    match lux {
        x if x < 10.0 => 20,
        x if x < 50.0 => 30,
        x if x < 100.0 => 40,
        x if x < 200.0 => 50,
        x if x < 500.0 => 65,
        x if x < 1000.0 => 80,
        _ => 100,
    }
}

fn calculate_contrast_from_lux(lux: f64) -> u32 {
    match lux {
        x if x < 10.0 => 40,
        x if x < 50.0 => 45,
        x if x < 100.0 => 55,
        x if x < 200.0 => 65,
        x if x < 500.0 => 75,
        x if x < 1000.0 => 85,
        _ => 95,
    }
}

async fn refresh_monitors(state: &AppState) -> Result<(), String> {
    match modules::monitor::get_monitor_list_sync() {
        Ok(monitor_list) => {
            let mut monitors = state.monitors.lock().unwrap();
            *monitors = monitor_list;
            Ok(())
        }
        Err(e) => Err(e)
    }
}

fn sync_autostart_state(state: &AppState) -> Result<(), String> {
    let registry_state = check_autostart_enabled();
    let current_app_state = *state.autostart_enabled.lock().unwrap();
    
    if registry_state != current_app_state {
        debug!("Autostart state desynchronized - registry={}, app={}", registry_state, current_app_state);
        
        // Update app state to match registry (registry is source of truth)
        {
            let mut autostart = state.autostart_enabled.lock().unwrap();
            *autostart = registry_state;
        }
        
        // Update config to match registry
        {
            let mut config = state.config.lock().unwrap();
            config.autostart_enabled = registry_state;
            config.save()?;
        }
        
        info!("Autostart state synchronized with registry: {}", if registry_state { "enabled" } else { "disabled" });
    }
    
    Ok(())
}

async fn adjust_display_settings_for_monitors(state: &AppState, target_brightness: u32, target_contrast: u32) -> Result<(), String> {
    let mut monitors = match state.monitors.lock() {
        Ok(guard) => guard.clone(),
        Err(_) => {
            error!("Monitor lock poisoned");
            return Err("Monitor lock poisoned".to_string());
        }
    };
    
    let mut success_count = 0;
    let mut hardware_write_count = 0;
    let mut last_error = String::new();
    
    for (index, monitor) in monitors.iter_mut().enumerate() {
        match modules::monitor::set_monitor_settings_with_cache(monitor, target_brightness, target_contrast) {
            Ok(hardware_written) => {
                success_count += 1;
                if hardware_written {
                    hardware_write_count += 1;
                    debug!("Hardware write performed for monitor {}", index);
                } else {
                    debug!("Monitor {} already at target values - no hardware write needed", index);
                }
            }
            Err(e) => {
                error!("Failed to update monitor {}: {}", index, e);
                last_error = e;
            }
        }
    }
    
    // Update cached monitors in state
    if let Ok(mut state_monitors) = state.monitors.lock() {
        *state_monitors = monitors;
    }
    
    // Update last brightness only if at least one monitor succeeded
    if success_count > 0 {
        if let Ok(mut last_brightness) = state.last_brightness.lock() {
            *last_brightness = target_brightness;
        }
        
        if hardware_write_count > 0 {
            // Update hardware write tracking
            if let Ok(mut last_write) = state.last_hardware_write.lock() {
                *last_write = std::time::Instant::now();
            }
            if let Ok(mut write_count) = state.hardware_write_count.lock() {
                *write_count += hardware_write_count as u32;
                
                // Hardware wear warning
                if *write_count > 1000 {
                    error!("WARNING: {} DDC/CI writes performed - monitor EEPROM wear risk!", *write_count);
                } else if *write_count % 100 == 0 {
                    info!("Hardware protection: {} total DDC/CI writes since start", *write_count);
                }
            }
            
            info!("Hardware protection: {} monitors updated, {} required hardware writes", success_count, hardware_write_count);
        }
        
        Ok(())
    } else {
        Err(format!("Failed to update any monitors. Last error: {}", last_error))
    }
}

fn start_monitor_refresh_task(state: AppState) -> thread::JoinHandle<()> {
    thread::spawn(move || {
        let mut last_refresh = Instant::now();
        
        loop {
            if state.should_exit.lock().map_or(true, |exit| *exit) {
                info!("Monitor refresh task shutting down...");
                break;
            }
            
            // Refresh monitors every 5 minutes
            if last_refresh.elapsed() >= Duration::from_secs(300) {
                state.runtime.block_on(async {
                    if let Err(e) = refresh_monitors(&state).await {
                        error!("Failed to refresh monitors: {}", e);
                    } else {
                        debug!("Monitors refreshed automatically");
                    }
                });
                
                // Also sync autostart state periodically
                if let Err(e) = sync_autostart_state(&state) {
                    error!("Failed to sync autostart state: {}", e);
                }
                
                last_refresh = Instant::now();
            }
            
            thread::sleep(Duration::from_secs(30));
        }
    })
}

async fn set_brightness_from_ambient_light(state: &AppState) -> Result<(), String> {
    match modules::sensor::get_light_sensor_lux_sync() {
        Ok(lux) => {
            debug!("Light sensor reading: {:.1} lux", lux);
            
            let target_brightness = calculate_brightness_from_lux(lux);
            let target_contrast = calculate_contrast_from_lux(lux);
            
            if let Err(e) = adjust_display_settings_for_monitors(state, target_brightness, target_contrast).await {
                return Err(e);
            }
            
            // Update config
            if let Ok(mut config) = state.config.lock() {
                config.last_brightness = target_brightness;
                if let Err(e) = config.save() {
                    error!("Failed to save brightness to config: {}", e);
                }
            } else {
                error!("Failed to lock config for saving brightness");
            }
            
            info!("Brightness set from ambient light: {}% brightness, {}% contrast based on {:.1} lux",
                   target_brightness, target_contrast, lux);
            Ok(())
        }
        Err(e) => {
            Err(format!("Failed to read light sensor: {}", e))
        }
    }
}

fn load_icon() -> Result<Icon, Box<dyn std::error::Error>> {
    let icon_data = include_bytes!("icons/32x32.png");
    let image = image::load_from_memory_with_format(icon_data, ImageFormat::Png)?;
    let rgba = image.to_rgba8();
    let (width, height) = rgba.dimensions();
    
    Ok(Icon::from_rgba(rgba.into_raw(), width, height)?)
}

fn create_tray_menu(state: &AppState) -> Result<(Menu, CheckMenuItem), Box<dyn std::error::Error>> {
    let autostart_enabled = *state.autostart_enabled.lock().unwrap();

    let ambient_light_item = MenuItem::with_id("ambient_light", "Set from Ambient Light", true, None);
    // Use a checkbox for autostart; checked reflects enabled state
    let autostart_check = CheckMenuItem::with_id("autostart", "Autostart", true, autostart_enabled, None);

    // Create brightness profiles submenu
    let config = state.config.lock().unwrap();
    let profiles = config.brightness_profiles.clone();
    drop(config);

    let brightness_submenu = Submenu::with_id("brightness_submenu", "Set Brightness", true);
    for profile in &profiles {
        let profile_item = MenuItem::with_id(profile.name.to_lowercase(), &profile.name, true, None);
        brightness_submenu.append(&profile_item)?;
    }

    let refresh_item = MenuItem::with_id("refresh", "Refresh Monitors", true, None);
    let quit_item = MenuItem::with_id("quit", "Exit", true, None);

    let menu = Menu::new();
    menu.append(&ambient_light_item)?;
    menu.append(&autostart_check)?;
    menu.append(&brightness_submenu)?;
    menu.append(&refresh_item)?;
    menu.append(&quit_item)?;

    Ok((menu, autostart_check))
}



fn set_brightness_from_ambient_light_sync(state: &AppState) -> Result<(), String> {
    let runtime = state.runtime.clone();
    runtime.block_on(set_brightness_from_ambient_light(state))
}

fn toggle_autostart(state: &AppState) -> Result<bool, String> {
    // Check current registry state (source of truth)
    let current_registry_state = check_autostart_enabled();
    let target_state = !current_registry_state;
    
    debug!("Toggling autostart: registry={} -> target={}", current_registry_state, target_state);
    
    let result = if target_state {
        enable_autostart()
    } else {
        disable_autostart()
    };
    
    match result {
        Ok(()) => {
            // Verify the registry change was successful
            let new_registry_state = check_autostart_enabled();
            if new_registry_state != target_state {
                return Err(format!("Registry change verification failed: expected={}, actual={}", target_state, new_registry_state));
            }
            
            // Update app state to match registry
            {
                let mut autostart = state.autostart_enabled.lock().unwrap();
                *autostart = new_registry_state;
            }
            
            // Update config to match registry
            {
                let mut config = state.config.lock().unwrap();
                config.autostart_enabled = new_registry_state;
                config.save()?;
            }
            
            info!("Autostart toggled successfully: {}", if new_registry_state { "enabled" } else { "disabled" });
            Ok(new_registry_state)
        }
        Err(e) => {
            Err(format!("Failed to {} autostart: {}", if target_state { "enable" } else { "disable" }, e))
        }
    }
}

fn enable_autostart() -> Result<(), String> {
    let hkcu = RegKey::predef(HKEY_CURRENT_USER);
    
    // Create or open the key with write access
    let (key, _) = hkcu.create_subkey("SOFTWARE\\Microsoft\\Windows\\CurrentVersion\\Run")
        .map_err(|e| format!("Failed to create/open registry key: {}", e))?;
    
    let exe_path = std::env::current_exe()
        .map_err(|e| format!("Failed to get executable path: {}", e))?;
    
    let registry_value = format!("\"{}\"", exe_path.display());
    debug!("Setting autostart registry value: {}", registry_value);
    
    key.set_value("XCreen", &registry_value)
        .map_err(|e| format!("Failed to set registry value: {}", e))?;
    
    debug!("Autostart registry entry created successfully");
    Ok(())
}

fn disable_autostart() -> Result<(), String> {
    let hkcu = RegKey::predef(HKEY_CURRENT_USER);
    
    // Open the key with write access for deletion
    let key = hkcu.open_subkey_with_flags("SOFTWARE\\Microsoft\\Windows\\CurrentVersion\\Run", KEY_SET_VALUE | KEY_QUERY_VALUE)
        .map_err(|e| format!("Failed to open registry key with write access: {}", e))?;
    
    // Check if the value exists before trying to delete it
    match key.get_value::<String, _>("XCreen") {
        Ok(_) => {
            // Value exists, delete it
            debug!("Deleting XCreen autostart registry entry");
            key.delete_value("XCreen")
                .map_err(|e| format!("Failed to delete registry value: {}", e))?;
            debug!("XCreen autostart registry entry deleted successfully");
        }
        Err(_) => {
            // Value doesn't exist, which is fine - autostart is already disabled
            debug!("XCreen registry value not found, autostart already disabled");
        }
    }
    
    Ok(())
}

fn check_autostart_enabled() -> bool {
    let hkcu = RegKey::predef(HKEY_CURRENT_USER);
    match hkcu.open_subkey_with_flags("SOFTWARE\\Microsoft\\Windows\\CurrentVersion\\Run", KEY_QUERY_VALUE) {
        Ok(key) => {
            match key.get_value::<String, _>("XCreen") {
                Ok(value) => {
                    debug!("Autostart registry entry found: {}", value);
                    true
                }
                Err(e) => {
                    debug!("Autostart registry entry not found: {}", e);
                    false
                }
            }
        }
        Err(e) => {
            debug!("Failed to open autostart registry key for read: {}", e);
            false
        }
    }
}

async fn set_brightness_profile(state: &AppState, profile_name: String) -> Result<(), String> {
    let (brightness, contrast) = {
        let config = state.config.lock().unwrap();
        let profile = config.brightness_profiles.iter()
            .find(|p| p.name.to_lowercase() == profile_name.to_lowercase())
            .ok_or_else(|| format!("Profile '{}' not found", profile_name))?;
        
        (profile.brightness, profile.contrast)
    };
    
    if let Err(e) = adjust_display_settings_for_monitors(state, brightness, contrast).await {
        return Err(e);
    }
    
    // Update config
    {
        let mut config = state.config.lock().unwrap();
        config.last_brightness = brightness;
        if let Err(e) = config.save() {
            error!("Failed to save brightness to config: {}", e);
        }
    }
    
    info!("Applied brightness profile '{}': {}% brightness, {}% contrast", profile_name, brightness, contrast);
    Ok(())
}

fn handle_menu_event(state: &AppState, proxy: &EventLoopProxy<AppEvent>, menu_id: &str) {
    match menu_id {
        "ambient_light" => {
            match set_brightness_from_ambient_light_sync(state) {
                Ok(()) => {
                    info!("Brightness set from ambient light successfully");
                }
                Err(e) => {
                    error!("Failed to set brightness from ambient light: {}", e);
                }
            }
        }
        "autostart" => {
            match toggle_autostart(state) {
                Ok(enabled) => {
                    info!("Autostart toggled: {}", if enabled { "enabled" } else { "disabled" });
                    // Ask main thread to update the checkbox checked state
                    if let Err(e) = proxy.send_event(AppEvent::SetAutostartChecked(enabled)) {
                        error!("Failed to send SetAutostartChecked event: {}", e);
                    }
                }
                Err(e) => {
                    error!("Failed to toggle autostart: {}", e);
                }
            }
        }
        "refresh" => {
            info!("Refreshing monitors...");
            let state_clone = state.clone();
            let runtime = state_clone.runtime.clone();
            thread::spawn(move || {
                runtime.block_on(async move {
                    if let Err(e) = refresh_monitors(&state_clone).await {
                        error!("Failed to refresh monitors: {}", e);
                    } else {
                        info!("Monitors refreshed successfully");
                    }
                });
            });
        }
        "quit" => {
            info!("Exiting XCreen...");
            if let Ok(mut should_exit) = state.should_exit.lock() {
                *should_exit = true;
            }

            // Give background tasks time to shutdown gracefully
            thread::spawn(move || {
                thread::sleep(Duration::from_millis(500));
                std::process::exit(0);
            });
        }
        "brightness_submenu" => {
            // Ignore submenu clicks
            debug!("Brightness submenu clicked (ignored)");
        }
        profile_name => {
            // Check if this is a valid brightness profile before trying to apply it
            let profile_exists = {
                let config = state.config.lock().unwrap();
                config.brightness_profiles.iter()
                    .any(|p| p.name.to_lowercase() == profile_name.to_lowercase())
            };
            
            if profile_exists {
                // Handle brightness profile selection
                let profile_name = profile_name.to_string();
                let state_clone = state.clone();
                let runtime = state_clone.runtime.clone();
                thread::spawn(move || {
                    runtime.block_on(async move {
                        if let Err(e) = set_brightness_profile(&state_clone, profile_name.clone()).await {
                            error!("Failed to apply brightness profile '{}': {}", profile_name, e);
                        }
                    });
                });
            } else {
                debug!("Unknown menu item clicked: '{}'", profile_name);
            }
        }
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Load config
    let config = AppConfig::load().unwrap_or_else(|e| {
        eprintln!("Failed to load config, using defaults: {}", e);
        AppConfig::default()
    });

    // Initialize logger
    if let Err(e) = logger::init_logger(&config.log_level) {
        eprintln!("Failed to initialize logger: {}", e);
        eprintln!("Continuing without file logging...");
    } else {
        debug!("Logger initialized successfully with level: {}", config.log_level);
    }

    let app_state = AppState::new(config.clone())?;

    // Check for command line arguments
    let args: Vec<String> = std::env::args().collect();
    let start_minimized = args.contains(&"--minimized".to_string());
    let show_help = args.contains(&"--help".to_string()) || args.contains(&"-h".to_string());
    
    if show_help {
        println!("XCreen - Adaptive Brightness Control");
        println!("Usage: xcreen [OPTIONS]");
        println!("Options:");
        println!("  --minimized    Start minimized to system tray");
        println!("  --help, -h     Show this help message");
        return Ok(());
    }

    // Initialize monitors
    if let Err(e) = refresh_monitors(&app_state).await {
        error!("Failed to initialize monitors: {}", e);
        error!("Application may not function correctly without monitor access");
    } else {
        let monitor_count = app_state.monitors.lock().unwrap().len();
        info!("Successfully initialized {} monitor(s)", monitor_count);
    }

    // Sync autostart state with registry (registry is source of truth)
    let registry_autostart = check_autostart_enabled();
    let config_autostart = {
        let config = app_state.config.lock().unwrap();
        config.autostart_enabled
    };
    
    info!("Autostart state check: registry={}, config={}", registry_autostart, config_autostart);
    
    if registry_autostart != config_autostart {
        info!("Syncing autostart state to match registry (source of truth)");
        // Update config to match registry state (registry is source of truth)
        {
            let mut config = app_state.config.lock().unwrap();
            config.autostart_enabled = registry_autostart;
            if let Err(e) = config.save() {
                error!("Failed to save synced autostart state: {}", e);
            } else {
                debug!("Config autostart state updated to match registry: {}", registry_autostart);
            }
        }
        // Update app state
        {
            let mut autostart = app_state.autostart_enabled.lock().unwrap();
            *autostart = registry_autostart;
        }
    } else {
        debug!("Autostart state already synchronized");
    }

    // Start background tasks
    let _monitor_refresh_handle = start_monitor_refresh_task(app_state.clone());

    // Create event loop (with user events) and tray
    let event_loop = EventLoopBuilder::<AppEvent>::with_user_event().build().map_err(|e| {
        error!("Failed to create event loop: {}", e);
        e
    })?;
    let proxy = event_loop.create_proxy();
    
    let icon = load_icon().map_err(|e| {
        error!("Failed to load tray icon: {}", e);
        e
    })?;
    
    let (initial_menu, autostart_item) = create_tray_menu(&app_state).map_err(|e| {
        error!("Failed to create tray menu: {}", e);
        e
    })?;

    let _tray = TrayIconBuilder::new()
        .with_menu(Box::new(initial_menu))
        .with_tooltip("XCreen - Adaptive Brightness Control")
        .with_icon(icon)
        .build().map_err(|e| {
            error!("Failed to create system tray icon: {}", e);
            e
        })?;
    
    info!("System tray icon created successfully");

    // Start config file watcher (after proxy and tray are set up)
    let proxy_for_watcher = proxy.clone();
    let _config_watcher = match watcher::start_config_watcher(
        app_state.config.clone(),
        app_state.autostart_enabled.clone(),
        app_state.last_brightness.clone(),
        Some(Arc::new(move |cfg: &AppConfig| {
            // Push UI update for autostart checkbox when config reloads
            let enabled = cfg.autostart_enabled;
            if let Err(e) = proxy_for_watcher.send_event(AppEvent::SetAutostartChecked(enabled)) {
                error!("Failed to send SetAutostartChecked from watcher: {}", e);
            } else {
                debug!("Sent SetAutostartChecked({}) from watcher", enabled);
            }
        })),
    ) {
        Ok(watcher) => {
            info!("Config file watcher started successfully");
            Some(watcher)
        }
        Err(e) => {
            error!("Failed to start config file watcher: {}", e);
            error!("Config changes will not be detected automatically");
            None
        }
    };

    if !start_minimized {
        info!("Adaptive brightness control started");
    }

    info!("XCreen started successfully");

    // Handle menu events
    let menu_channel = tray_icon::menu::MenuEvent::receiver();
    let app_state_for_menu = app_state.clone();
    let proxy_for_menu = proxy.clone();

    thread::spawn(move || {
        debug!("Menu event handler thread started");
        loop {
            match menu_channel.recv() {
                Ok(event) => {
                    debug!("Received menu event: {}", event.id().0);
                    handle_menu_event(&app_state_for_menu, &proxy_for_menu, &event.id().0);
                }
                Err(e) => {
                    error!("Failed to receive menu event: {}", e);
                    break;
                }
            }
        }
        debug!("Menu event handler thread ended");
    });

    // Handle tray icon events
    let tray_event_receiver = TrayIconEvent::receiver();

    thread::spawn(move || {
        debug!("Tray icon event handler thread started");
        loop {
            match tray_event_receiver.recv() {
                Ok(event) => {
                    match event {
                        TrayIconEvent::Click { button, button_state, .. } => {
                            if button == MouseButton::Left && button_state == MouseButtonState::Down {
                                debug!("Left click on tray icon");
                            }
                        }
                        _ => {
                            debug!("Other tray icon event received: {:?}", event);
                        }
                    }
                }
                Err(e) => {
                    error!("Failed to receive tray icon event: {}", e);
                    break;
                }
            }
        }
        debug!("Tray icon event handler thread ended");
    });

    // Run event loop
    debug!("Starting main event loop");
    event_loop.run(move |event, event_loop| {
        event_loop.set_control_flow(ControlFlow::Wait);
        
        match event {
            winit::event::Event::UserEvent(AppEvent::SetAutostartChecked(checked)) => {
                debug!("Handling SetAutostartChecked({}) on main thread", checked);
                // Update checkbox to reflect new autostart state
                autostart_item.set_checked(checked);
            }
            winit::event::Event::WindowEvent { event: winit::event::WindowEvent::CloseRequested, .. } => {
                info!("Window close requested, exiting event loop");
                event_loop.exit();
            }
            _ => {}
        }
    }).map_err(|e| {
        error!("Event loop error: {}", e);
        e
    })?;

    Ok(())
}
