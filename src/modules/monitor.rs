use serde::{Deserialize, Serialize};
use std::sync::OnceLock;
use windows::{
    Win32::{
        Foundation::{BOOL, LPARAM, RECT},
        Graphics::Gdi::{EnumDisplayMonitors, GetMonitorInfoW, HDC, HMONITOR, MONITORINFOEXW},
        System::LibraryLoader::{GetProcAddress, LoadLibraryA},
    },
    core::s,
};

// DDC/CI function types
type GetNumPhysicalMonitorsFn = unsafe extern "system" fn(HMONITOR, *mut u32) -> BOOL;
type GetPhysicalMonitorsFn =
    unsafe extern "system" fn(HMONITOR, u32, *mut std::ffi::c_void) -> BOOL;
type GetMonitorBrightnessFn =
    unsafe extern "system" fn(isize, *mut u32, *mut u32, *mut u32) -> BOOL;
type SetMonitorBrightnessFn = unsafe extern "system" fn(isize, u32) -> BOOL;
type GetMonitorContrastFn = unsafe extern "system" fn(isize, *mut u32, *mut u32, *mut u32) -> BOOL;
type SetMonitorContrastFn = unsafe extern "system" fn(isize, u32) -> BOOL;
type DestroyPhysicalMonitorFn = unsafe extern "system" fn(isize) -> BOOL;

// Cached DDC/CI functions for performance
struct DdcFunctions {
    get_num_physical_monitors: GetNumPhysicalMonitorsFn,
    get_physical_monitors: GetPhysicalMonitorsFn,
    get_monitor_brightness: GetMonitorBrightnessFn,
    set_monitor_brightness: SetMonitorBrightnessFn,
    get_monitor_contrast: GetMonitorContrastFn,
    set_monitor_contrast: SetMonitorContrastFn,
    destroy_physical_monitor: DestroyPhysicalMonitorFn,
}

static DDC_FUNCTIONS: OnceLock<Result<DdcFunctions, String>> = OnceLock::new();

fn get_ddc_functions() -> Result<&'static DdcFunctions, &'static str> {
    DDC_FUNCTIONS
        .get_or_init(|| unsafe {
            let dxva2_lib =
                LoadLibraryA(s!("dxva2.dll")).map_err(|_| "DDC/CI not supported".to_string())?;

            let get_num_physical_monitors =
                GetProcAddress(dxva2_lib, s!("GetNumberOfPhysicalMonitorsFromHMONITOR"))
                    .ok_or("Missing GetNumberOfPhysicalMonitorsFromHMONITOR".to_string())?;
            let get_physical_monitors =
                GetProcAddress(dxva2_lib, s!("GetPhysicalMonitorsFromHMONITOR"))
                    .ok_or("Missing GetPhysicalMonitorsFromHMONITOR".to_string())?;
            let get_monitor_brightness = GetProcAddress(dxva2_lib, s!("GetMonitorBrightness"))
                .ok_or("Missing GetMonitorBrightness".to_string())?;
            let set_monitor_brightness = GetProcAddress(dxva2_lib, s!("SetMonitorBrightness"))
                .ok_or("Missing SetMonitorBrightness".to_string())?;
            let get_monitor_contrast = GetProcAddress(dxva2_lib, s!("GetMonitorContrast"))
                .ok_or("Missing GetMonitorContrast".to_string())?;
            let set_monitor_contrast = GetProcAddress(dxva2_lib, s!("SetMonitorContrast"))
                .ok_or("Missing SetMonitorContrast".to_string())?;
            let destroy_physical_monitor = GetProcAddress(dxva2_lib, s!("DestroyPhysicalMonitor"))
                .ok_or("Missing DestroyPhysicalMonitor".to_string())?;

            Ok(DdcFunctions {
                get_num_physical_monitors: std::mem::transmute::<
                    unsafe extern "system" fn() -> isize,
                    GetNumPhysicalMonitorsFn,
                >(get_num_physical_monitors),
                get_physical_monitors: std::mem::transmute::<
                    unsafe extern "system" fn() -> isize,
                    GetPhysicalMonitorsFn,
                >(get_physical_monitors),
                get_monitor_brightness: std::mem::transmute::<
                    unsafe extern "system" fn() -> isize,
                    GetMonitorBrightnessFn,
                >(get_monitor_brightness),
                set_monitor_brightness: std::mem::transmute::<
                    unsafe extern "system" fn() -> isize,
                    SetMonitorBrightnessFn,
                >(set_monitor_brightness),
                get_monitor_contrast: std::mem::transmute::<
                    unsafe extern "system" fn() -> isize,
                    GetMonitorContrastFn,
                >(get_monitor_contrast),
                set_monitor_contrast: std::mem::transmute::<
                    unsafe extern "system" fn() -> isize,
                    SetMonitorContrastFn,
                >(set_monitor_contrast),
                destroy_physical_monitor: std::mem::transmute::<
                    unsafe extern "system" fn() -> isize,
                    DestroyPhysicalMonitorFn,
                >(destroy_physical_monitor),
            })
        })
        .as_ref()
        .map_err(|e| e.as_str())
}

#[repr(C)]
struct PhysicalMonitor {
    handle: isize,           // HMONITOR handle (HANDLE)
    description: [u16; 128], // WCHAR description
}

fn enumerate_physical_monitors(
    monitor_handle: isize,
) -> Result<Vec<PhysicalMonitor>, &'static str> {
    let ddc = get_ddc_functions()?;
    let hmonitor = HMONITOR(monitor_handle);

    unsafe {
        let mut num_monitors = 0u32;
        if !(ddc.get_num_physical_monitors)(hmonitor, &mut num_monitors).as_bool()
            || num_monitors == 0
        {
            return Err("No physical monitors found");
        }

        let mut monitors: Vec<PhysicalMonitor> = Vec::with_capacity(num_monitors as usize);
        // Fill the buffer; set length only on success to avoid dropping uninitialized values
        let ok = (ddc.get_physical_monitors)(
            hmonitor,
            num_monitors,
            monitors.as_mut_ptr() as *mut std::ffi::c_void,
        )
        .as_bool();
        if !ok {
            return Err("Failed to get physical monitors");
        }
        monitors.set_len(num_monitors as usize);

        Ok(monitors)
    }
}

#[derive(Serialize, Deserialize, Clone)]
pub struct MonitorInfo {
    pub id: String,
    pub display_handle: isize,
    pub display_device: String,
    pub physical_index: u32,
    pub name: String,
    pub is_primary: bool,
    pub cached_brightness: Option<u32>,
    pub cached_contrast: Option<u32>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MonitorLevels {
    pub brightness: u32,
    pub contrast: u32,
}

fn description_to_string(description: &[u16; 128]) -> String {
    String::from_utf16_lossy(
        &description[..description
            .iter()
            .position(|&x| x == 0)
            .unwrap_or(description.len())],
    )
}

fn stable_monitor_id(display_device: &str, physical_index: u32, description: &str) -> String {
    let source = format!("{}|{}|{}", display_device, physical_index, description);
    source
        .as_bytes()
        .iter()
        .map(|byte| format!("{:02x}", byte))
        .collect()
}

pub fn get_monitor_list_sync() -> Result<Vec<MonitorInfo>, String> {
    let mut monitors = Vec::new();

    unsafe extern "system" fn enum_monitor_proc(
        hmonitor: HMONITOR,
        _hdc: HDC,
        _rect: *mut RECT,
        lparam: LPARAM,
    ) -> BOOL {
        let monitors_ptr = lparam.0 as *mut Vec<MonitorInfo>;
        let monitors = unsafe { &mut *monitors_ptr };

        let mut monitor_info = MONITORINFOEXW {
            monitorInfo: Default::default(),
            szDevice: [0; 32],
        };
        monitor_info.monitorInfo.cbSize = std::mem::size_of::<MONITORINFOEXW>() as u32;

        if unsafe {
            GetMonitorInfoW(hmonitor, &mut monitor_info.monitorInfo as *mut _ as *mut _).as_bool()
        } {
            let device_name = String::from_utf16_lossy(
                &monitor_info.szDevice[..monitor_info
                    .szDevice
                    .iter()
                    .position(|&x| x == 0)
                    .unwrap_or(32)],
            );

            if let Ok(mut physical_monitors) = enumerate_physical_monitors(hmonitor.0) {
                let is_primary = (monitor_info.monitorInfo.dwFlags & 1) != 0;

                for (physical_index, physical_monitor) in physical_monitors.iter().enumerate() {
                    let description = description_to_string(&physical_monitor.description);
                    let name = if description.trim().is_empty() {
                        device_name.clone()
                    } else {
                        description.clone()
                    };

                    monitors.push(MonitorInfo {
                        id: stable_monitor_id(&device_name, physical_index as u32, &description),
                        display_handle: hmonitor.0,
                        display_device: device_name.clone(),
                        physical_index: physical_index as u32,
                        name,
                        is_primary,
                        cached_brightness: None,
                        cached_contrast: None,
                    });
                }

                if let Ok(ddc) = get_ddc_functions() {
                    for physical_monitor in &physical_monitors {
                        let _ = unsafe { (ddc.destroy_physical_monitor)(physical_monitor.handle) };
                    }
                }
                physical_monitors.clear();
            }
        }

        BOOL::from(true)
    }

    unsafe {
        let _ = EnumDisplayMonitors(
            HDC::default(),
            None,
            Some(enum_monitor_proc),
            LPARAM(&mut monitors as *mut _ as isize),
        );
    }

    Ok(monitors)
}

// Hardware wear protection - only write if values actually changed
pub fn set_monitor_settings_with_cache(
    monitor_info: &mut MonitorInfo,
    brightness: u32,
    contrast: u32,
) -> Result<bool, String> {
    if brightness > 100 || contrast > 100 {
        return Err("Values must be between 0 and 100".to_string());
    }

    // Check if we actually need to write to hardware
    let brightness_changed = monitor_info.cached_brightness != Some(brightness);
    let contrast_changed = monitor_info.cached_contrast != Some(contrast);

    if !brightness_changed && !contrast_changed {
        // No hardware write needed - values are already set!
        return Ok(false);
    }

    // Only write to hardware if relevant values changed
    let result = set_monitor_settings_sync_internal(
        monitor_info.display_handle,
        monitor_info.physical_index,
        brightness,
        contrast,
        brightness_changed,
        contrast_changed,
    );
    match result {
        Ok(apply) => {
            if apply.brightness_succeeded {
                monitor_info.cached_brightness = Some(brightness);
            }
            if apply.contrast_succeeded {
                monitor_info.cached_contrast = Some(contrast);
            }
            Ok(apply.brightness_wrote || apply.contrast_wrote)
        }
        Err(e) => Err(e),
    }
}

/// Read the monitor's current DDC/CI brightness and contrast as percentages.
pub fn get_monitor_levels(monitor_info: &mut MonitorInfo) -> Result<MonitorLevels, String> {
    let ddc = get_ddc_functions().map_err(|e| format!("DDC/CI error: {e}"))?;
    let physical_monitors = enumerate_physical_monitors(monitor_info.display_handle)
        .map_err(|e| format!("Failed to get physical monitor(s): {e}"))?;

    let result = (|| {
        let pm = physical_monitors
            .get(monitor_info.physical_index as usize)
            .ok_or_else(|| {
                format!(
                    "Physical monitor index {} no longer exists",
                    monitor_info.physical_index
                )
            })?;

        let to_percent = |value: u32, min: u32, max: u32| -> u32 {
            if max <= min {
                return 0;
            }
            (((value.saturating_sub(min)) as u64 * 100 + ((max - min) as u64 / 2))
                / (max - min) as u64) as u32
        };

        unsafe {
            let mut min_b = 0;
            let mut cur_b = 0;
            let mut max_b = 0;
            if !(ddc.get_monitor_brightness)(pm.handle, &mut min_b, &mut cur_b, &mut max_b)
                .as_bool()
            {
                return Err("Brightness is not supported by this monitor".to_string());
            }

            let mut min_c = 0;
            let mut cur_c = 0;
            let mut max_c = 0;
            if !(ddc.get_monitor_contrast)(pm.handle, &mut min_c, &mut cur_c, &mut max_c).as_bool()
            {
                return Err("Contrast is not supported by this monitor".to_string());
            }

            Ok(MonitorLevels {
                brightness: to_percent(cur_b, min_b, max_b).min(100),
                contrast: to_percent(cur_c, min_c, max_c).min(100),
            })
        }
    })();

    unsafe {
        for pm in &physical_monitors {
            let _ = (ddc.destroy_physical_monitor)(pm.handle);
        }
    }

    if let Ok(levels) = result {
        monitor_info.cached_brightness = Some(levels.brightness);
        monitor_info.cached_contrast = Some(levels.contrast);
    }
    result
}

pub fn set_monitor_brightness_only(
    monitor_info: &mut MonitorInfo,
    brightness: u32,
) -> Result<bool, String> {
    let levels = get_monitor_levels(monitor_info)?;
    set_monitor_settings_with_cache(monitor_info, brightness, levels.contrast)
}

pub fn set_monitor_contrast_only(
    monitor_info: &mut MonitorInfo,
    contrast: u32,
) -> Result<bool, String> {
    let levels = get_monitor_levels(monitor_info)?;
    set_monitor_settings_with_cache(monitor_info, levels.brightness, contrast)
}

struct ApplyResult {
    brightness_succeeded: bool,
    contrast_succeeded: bool,
    brightness_wrote: bool,
    contrast_wrote: bool,
}

fn set_monitor_settings_sync_internal(
    monitor_handle: isize,
    physical_index: u32,
    brightness: u32,
    contrast: u32,
    apply_brightness: bool,
    apply_contrast: bool,
) -> Result<ApplyResult, String> {
    let ddc = get_ddc_functions().map_err(|e| format!("DDC/CI error: {}", e))?;

    // Enumerate all physical monitors for the given HMONITOR
    let mut physical_monitors = enumerate_physical_monitors(monitor_handle)
        .map_err(|e| format!("Failed to get physical monitor(s): {}", e))?;

    let mut any_success = false;
    let mut brightness_success = false;
    let mut contrast_success = false;
    let mut brightness_wrote = false;
    let mut contrast_wrote = false;
    let mut last_error: Option<String> = None;

    // Helper for rounding and clamping within range
    let scale_to_range = |value: u32, min_v: u32, max_v: u32| -> u32 {
        if max_v <= min_v {
            return min_v;
        }
        // Round to nearest
        let span = (max_v - min_v) as u64;
        let scaled = (value as u64 * span + 50) / 100; // +50 for rounding
        min_v + scaled as u32
    };

    unsafe {
        let Some(pm) = physical_monitors.get(physical_index as usize) else {
            for pm in &physical_monitors {
                let _ = (ddc.destroy_physical_monitor)(pm.handle);
            }
            return Err(format!(
                "Physical monitor index {} no longer exists",
                physical_index
            ));
        };

        {
            let handle = pm.handle;

            // Brightness
            if apply_brightness {
                let mut min_b = 0u32;
                let mut cur_b = 0u32;
                let mut max_b = 0u32;
                if (ddc.get_monitor_brightness)(handle, &mut min_b, &mut cur_b, &mut max_b)
                    .as_bool()
                {
                    let target_b = scale_to_range(brightness, min_b, max_b);
                    if target_b != cur_b {
                        if (ddc.set_monitor_brightness)(handle, target_b).as_bool() {
                            any_success = true;
                            brightness_success = true;
                            brightness_wrote = true;
                        } else {
                            last_error = Some("Failed to set brightness".to_string());
                        }
                    } else {
                        // No-op write avoided
                        any_success = true;
                        brightness_success = true;
                    }
                } else {
                    last_error = Some("Brightness not supported".to_string());
                }
            }

            // Contrast
            if apply_contrast {
                let mut min_c = 0u32;
                let mut cur_c = 0u32;
                let mut max_c = 0u32;
                if (ddc.get_monitor_contrast)(handle, &mut min_c, &mut cur_c, &mut max_c).as_bool()
                {
                    let target_c = scale_to_range(contrast, min_c, max_c);
                    if target_c != cur_c {
                        if (ddc.set_monitor_contrast)(handle, target_c).as_bool() {
                            any_success = true;
                            contrast_success = true;
                            contrast_wrote = true;
                        } else {
                            last_error = Some("Failed to set contrast".to_string());
                        }
                    } else {
                        // No-op write avoided
                        any_success = true;
                        contrast_success = true;
                    }
                } else {
                    last_error = Some("Contrast not supported".to_string());
                }
            }
        };

        // Always release handles to avoid resource leaks
        for pm in &physical_monitors {
            let _ = (ddc.destroy_physical_monitor)(pm.handle);
        }
        // Ensure vector is dropped after we destroy handles
        physical_monitors.clear();
    }

    if any_success || (!apply_brightness && !apply_contrast) {
        Ok(ApplyResult {
            brightness_succeeded: brightness_success,
            contrast_succeeded: contrast_success,
            brightness_wrote,
            contrast_wrote,
        })
    } else {
        Err(last_error.unwrap_or_else(|| "DDC/CI operation failed".to_string()))
    }
}
