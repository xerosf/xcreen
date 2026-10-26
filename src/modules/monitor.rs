use serde::{Deserialize, Serialize};
use std::sync::Mutex;
use std::thread;
use std::time::Duration;
use windows::{
    Win32::{
        Devices::Display::{
            DISPLAYCONFIG_DEVICE_INFO_GET_SOURCE_NAME, DISPLAYCONFIG_DEVICE_INFO_GET_TARGET_NAME,
            DISPLAYCONFIG_DEVICE_INFO_HEADER, DISPLAYCONFIG_MODE_INFO, DISPLAYCONFIG_PATH_INFO,
            DISPLAYCONFIG_SOURCE_DEVICE_NAME, DISPLAYCONFIG_TARGET_DEVICE_NAME,
            DisplayConfigGetDeviceInfo, QDC_ONLY_ACTIVE_PATHS, QueryDisplayConfig,
        },
        Foundation::{BOOL, ERROR_SUCCESS, LPARAM, RECT},
        Graphics::Gdi::{
            DISPLAYCONFIG_PATH_ACTIVE, EnumDisplayMonitors, GetMonitorInfoW, HDC, HMONITOR,
            MONITORINFOEXW,
        },
        System::LibraryLoader::{GetProcAddress, LoadLibraryA},
        UI::WindowsAndMessaging::MONITORINFOF_PRIMARY,
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

static DDC_FUNCTIONS: std::sync::OnceLock<Result<DdcFunctions, String>> =
    std::sync::OnceLock::new();
static DDC_LOCK: Mutex<()> = Mutex::new(());

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

fn with_ddc<T>(operation: impl FnOnce() -> T) -> T {
    let _guard = DDC_LOCK.lock().unwrap();
    operation()
}

#[repr(C)]
struct PhysicalMonitor {
    handle: isize,
    description: [u16; 128],
}

struct PhysicalMonitorList {
    monitors: Vec<PhysicalMonitor>,
}

impl PhysicalMonitorList {
    fn iter(&self) -> impl Iterator<Item = &PhysicalMonitor> {
        self.monitors.iter()
    }

    fn get(&self, index: usize) -> Option<&PhysicalMonitor> {
        self.monitors.get(index)
    }
}

impl Drop for PhysicalMonitorList {
    fn drop(&mut self) {
        if let Ok(ddc) = get_ddc_functions() {
            for monitor in &self.monitors {
                unsafe {
                    let _ = (ddc.destroy_physical_monitor)(monitor.handle);
                }
            }
        }
    }
}

fn enumerate_physical_monitors(monitor_handle: isize) -> Result<PhysicalMonitorList, &'static str> {
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
        Ok(PhysicalMonitorList { monitors })
    }
}

fn open_physical_monitors(monitor_handle: isize) -> Result<PhysicalMonitorList, String> {
    enumerate_physical_monitors(monitor_handle)
        .map_err(|err| format!("Failed to get physical monitor(s): {err}"))
}

/// The first DDC/CI call on a new handle often fails. Retry that call, then
/// reopen the handle once. Further retries only delay a display with no DDC/CI.
const DDC_CALL_TRIES: u32 = 3;
const DDC_SESSIONS: u32 = 2;
const DDC_RETRY_GAP: Duration = Duration::from_millis(80);

fn retry_sessions<T>(
    action: &str,
    mut call: impl FnMut() -> Result<T, String>,
) -> Result<T, String> {
    let mut last_error = String::new();
    for session in 1..=DDC_SESSIONS {
        match call() {
            Ok(value) => return Ok(value),
            Err(err) => {
                last_error = err;
                log::warn!("{action} failed ({session}/{DDC_SESSIONS}): {last_error}");
                if session < DDC_SESSIONS {
                    thread::sleep(DDC_RETRY_GAP);
                }
            }
        }
    }
    Err(last_error)
}

fn retry_bool(action: &str, mut call: impl FnMut() -> bool) -> bool {
    for attempt in 1..=DDC_CALL_TRIES {
        if call() {
            return true;
        }
        if attempt == DDC_CALL_TRIES {
            log::warn!("{action} did not succeed");
        } else {
            thread::sleep(DDC_RETRY_GAP);
        }
    }
    false
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
    #[serde(default, skip_serializing, skip_deserializing)]
    pub level_error: Option<String>,
}

impl MonitorInfo {
    pub fn cached_levels(&self) -> Option<MonitorLevels> {
        match (self.cached_brightness, self.cached_contrast) {
            (Some(brightness), Some(contrast)) => Some(MonitorLevels {
                brightness,
                contrast,
            }),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MonitorLevels {
    pub brightness: u32,
    pub contrast: u32,
}

fn wide_to_string(data: &[u16]) -> String {
    let end = data
        .iter()
        .position(|&unit| unit == 0)
        .unwrap_or(data.len());
    String::from_utf16_lossy(&data[..end]).trim().to_string()
}

fn is_placeholder_name(name: &str) -> bool {
    let trimmed = name.trim();
    trimmed.is_empty()
        || trimmed.eq_ignore_ascii_case("generic pnp monitor")
        || trimmed.to_ascii_lowercase().starts_with(r"\\.\display")
}

fn monitor_name(ddc_description: &str, edid_name: &str, gdi_device: &str) -> String {
    [ddc_description, edid_name]
        .into_iter()
        .find(|name| !is_placeholder_name(name))
        .or_else(|| {
            [edid_name, ddc_description, gdi_device]
                .into_iter()
                .find(|name| !name.trim().is_empty())
        })
        .unwrap_or(gdi_device)
        .trim()
        .to_string()
}

fn indexed_id(device_key: &str, physical_index: u32) -> String {
    if physical_index == 0 {
        device_key.to_string()
    } else {
        format!("{device_key}#{physical_index}")
    }
}

fn percent_from_range(value: u32, min: u32, max: u32) -> u32 {
    if max <= min {
        return 0;
    }
    let span = u64::from(max - min);
    let offset = u64::from(value.saturating_sub(min));
    ((offset * 100 + span / 2) / span).min(100) as u32
}

fn read_levels_from_handle(ddc: &DdcFunctions, handle: isize) -> Result<MonitorLevels, String> {
    unsafe {
        let mut min_b = 0;
        let mut cur_b = 0;
        let mut max_b = 0;
        if !retry_bool("Read brightness", || {
            (ddc.get_monitor_brightness)(handle, &mut min_b, &mut cur_b, &mut max_b).as_bool()
        }) {
            return Err("Brightness is not supported by this monitor".to_string());
        }

        let mut min_c = 0;
        let mut cur_c = 0;
        let mut max_c = 0;
        if !retry_bool("Read contrast", || {
            (ddc.get_monitor_contrast)(handle, &mut min_c, &mut cur_c, &mut max_c).as_bool()
        }) {
            return Err("Contrast is not supported by this monitor".to_string());
        }

        Ok(MonitorLevels {
            brightness: percent_from_range(cur_b, min_b, max_b),
            contrast: percent_from_range(cur_c, min_c, max_c),
        })
    }
}

fn read_physical_monitors_once(
    monitor_handle: isize,
) -> Result<Vec<(String, MonitorLevels)>, String> {
    let ddc = get_ddc_functions().map_err(|err| format!("DDC/CI error: {err}"))?;
    let physical = open_physical_monitors(monitor_handle)?;
    let mut values = Vec::new();
    let mut last_error = None;
    for monitor in physical.iter() {
        let description = wide_to_string(&monitor.description);
        match read_levels_from_handle(ddc, monitor.handle) {
            Ok(levels) => values.push((description, levels)),
            Err(err) => last_error = Some(err),
        }
    }
    if values.is_empty() {
        return Err(last_error.unwrap_or_else(|| "No physical monitors found".to_string()));
    }
    Ok(values)
}

fn read_physical_monitors(monitor_handle: isize) -> Result<Vec<(String, MonitorLevels)>, String> {
    retry_sessions("Monitor control read", || {
        read_physical_monitors_once(monitor_handle)
    })
}

struct GdiDisplay {
    handle: isize,
    device: String,
    is_primary: bool,
}

#[derive(Clone)]
struct DisplayIdentity {
    gdi_device: String,
    friendly_name: String,
    device_path: String,
    edid_manufacturer: u16,
    edid_product: u16,
    connector_instance: u32,
}

fn enumerate_gdi_displays() -> Vec<GdiDisplay> {
    let mut displays = Vec::new();

    unsafe extern "system" fn enum_monitor_proc(
        hmonitor: HMONITOR,
        _hdc: HDC,
        _rect: *mut RECT,
        lparam: LPARAM,
    ) -> BOOL {
        let displays = unsafe { &mut *(lparam.0 as *mut Vec<GdiDisplay>) };
        let mut info = MONITORINFOEXW {
            monitorInfo: Default::default(),
            szDevice: [0; 32],
        };
        info.monitorInfo.cbSize = std::mem::size_of::<MONITORINFOEXW>() as u32;
        if unsafe { GetMonitorInfoW(hmonitor, &mut info.monitorInfo as *mut _ as *mut _).as_bool() }
        {
            displays.push(GdiDisplay {
                handle: hmonitor.0,
                device: wide_to_string(&info.szDevice),
                is_primary: (info.monitorInfo.dwFlags & MONITORINFOF_PRIMARY) != 0,
            });
        }
        BOOL::from(true)
    }

    unsafe {
        let _ = EnumDisplayMonitors(
            HDC::default(),
            None,
            Some(enum_monitor_proc),
            LPARAM(&mut displays as *mut _ as isize),
        );
    }
    displays
}

fn query_display_identities() -> Vec<DisplayIdentity> {
    match query_display_identities_inner() {
        Ok(identities) => identities,
        Err(err) => {
            log::warn!("Display configuration query failed; monitor names may be generic: {err}");
            Vec::new()
        }
    }
}

fn query_display_identities_inner() -> Result<Vec<DisplayIdentity>, String> {
    unsafe {
        let mut path_count = 0u32;
        let mut mode_count = 0u32;
        let _ = QueryDisplayConfig(
            QDC_ONLY_ACTIVE_PATHS,
            &mut path_count,
            std::ptr::null_mut(),
            &mut mode_count,
            std::ptr::null_mut(),
            None,
        );
        if path_count == 0 {
            return Ok(Vec::new());
        }

        let mut paths = Vec::with_capacity(path_count as usize);
        for _ in 0..path_count {
            paths.push(std::mem::zeroed::<DISPLAYCONFIG_PATH_INFO>());
        }
        let mut modes = Vec::with_capacity(mode_count as usize);
        for _ in 0..mode_count {
            modes.push(std::mem::zeroed::<DISPLAYCONFIG_MODE_INFO>());
        }

        let mode_ptr = if modes.is_empty() {
            std::ptr::null_mut()
        } else {
            modes.as_mut_ptr()
        };
        let status = QueryDisplayConfig(
            QDC_ONLY_ACTIVE_PATHS,
            &mut path_count,
            paths.as_mut_ptr(),
            &mut mode_count,
            mode_ptr,
            None,
        );
        if status != ERROR_SUCCESS {
            return Err(format!("QueryDisplayConfig failed ({})", status.0));
        }
        paths.truncate(path_count as usize);

        let mut identities = Vec::new();
        for path in paths {
            if path.flags & DISPLAYCONFIG_PATH_ACTIVE == 0 {
                continue;
            }

            let mut source: DISPLAYCONFIG_SOURCE_DEVICE_NAME = std::mem::zeroed();
            source.header = DISPLAYCONFIG_DEVICE_INFO_HEADER {
                r#type: DISPLAYCONFIG_DEVICE_INFO_GET_SOURCE_NAME,
                size: std::mem::size_of::<DISPLAYCONFIG_SOURCE_DEVICE_NAME>() as u32,
                adapterId: path.sourceInfo.adapterId,
                id: path.sourceInfo.id,
            };
            if DisplayConfigGetDeviceInfo(&mut source.header) != 0 {
                continue;
            }

            let mut target: DISPLAYCONFIG_TARGET_DEVICE_NAME = std::mem::zeroed();
            target.header = DISPLAYCONFIG_DEVICE_INFO_HEADER {
                r#type: DISPLAYCONFIG_DEVICE_INFO_GET_TARGET_NAME,
                size: std::mem::size_of::<DISPLAYCONFIG_TARGET_DEVICE_NAME>() as u32,
                adapterId: path.targetInfo.adapterId,
                id: path.targetInfo.id,
            };
            if DisplayConfigGetDeviceInfo(&mut target.header) != 0 {
                continue;
            }

            identities.push(DisplayIdentity {
                gdi_device: wide_to_string(&source.viewGdiDeviceName),
                friendly_name: wide_to_string(&target.monitorFriendlyDeviceName),
                device_path: wide_to_string(&target.monitorDevicePath),
                edid_manufacturer: target.edidManufactureId,
                edid_product: target.edidProductCodeId,
                connector_instance: target.connectorInstance,
            });
        }
        Ok(identities)
    }
}

fn best_identity<'a>(
    identities: &'a [DisplayIdentity],
    gdi_device: &str,
) -> Option<&'a DisplayIdentity> {
    identities
        .iter()
        .filter(|identity| identity.gdi_device.eq_ignore_ascii_case(gdi_device))
        .min_by_key(|identity| is_placeholder_name(&identity.friendly_name))
}

fn monitor_id(identity: Option<&DisplayIdentity>, gdi_device: &str, physical_index: u32) -> String {
    if let Some(identity) = identity {
        if !identity.device_path.trim().is_empty() {
            return indexed_id(identity.device_path.trim(), physical_index);
        }
        if identity.edid_manufacturer != 0 || identity.edid_product != 0 {
            return indexed_id(
                &format!(
                    "edid:{:04X}:{:04X}:{}",
                    identity.edid_manufacturer, identity.edid_product, identity.connector_instance
                ),
                physical_index,
            );
        }
    }
    indexed_id(&format!("gdi:{gdi_device}"), physical_index)
}

fn remember_previous(
    previous: &[MonitorInfo],
    id: &str,
    name: &mut String,
    cached_brightness: &mut Option<u32>,
    cached_contrast: &mut Option<u32>,
    level_error: &mut Option<String>,
) {
    let Some(prior) = previous.iter().find(|monitor| monitor.id == id) else {
        return;
    };
    if is_placeholder_name(name) && !is_placeholder_name(&prior.name) {
        *name = prior.name.clone();
    }
    if cached_brightness.is_none() || cached_contrast.is_none() {
        if let (Some(brightness), Some(contrast)) = (prior.cached_brightness, prior.cached_contrast)
        {
            *cached_brightness = Some(brightness);
            *cached_contrast = Some(contrast);
            *level_error = None;
        }
    }
}

fn dedupe_monitors(monitors: Vec<MonitorInfo>) -> Vec<MonitorInfo> {
    let mut unique: Vec<MonitorInfo> = Vec::new();
    for monitor in monitors {
        if let Some(existing) = unique.iter_mut().find(|existing| existing.id == monitor.id) {
            if is_placeholder_name(&existing.name) && !is_placeholder_name(&monitor.name) {
                existing.name = monitor.name.clone();
            }
            if monitor.cached_levels().is_some() && existing.cached_levels().is_none() {
                let name = existing.name.clone();
                *existing = monitor;
                existing.name = name;
            }
        } else {
            unique.push(monitor);
        }
    }
    unique
}

fn describe_monitor(
    previous: &[MonitorInfo],
    display: &GdiDisplay,
    identity: Option<&DisplayIdentity>,
    physical_index: u32,
    description: &str,
    levels: Option<MonitorLevels>,
    mut level_error: Option<String>,
) -> MonitorInfo {
    let edid_name = identity
        .map(|item| item.friendly_name.as_str())
        .unwrap_or("");
    let id = monitor_id(identity, &display.device, physical_index);
    let mut name = monitor_name(description, edid_name, &display.device);
    let mut cached_brightness = levels.map(|levels| levels.brightness);
    let mut cached_contrast = levels.map(|levels| levels.contrast);
    remember_previous(
        previous,
        &id,
        &mut name,
        &mut cached_brightness,
        &mut cached_contrast,
        &mut level_error,
    );
    MonitorInfo {
        id,
        display_handle: display.handle,
        display_device: display.device.clone(),
        physical_index,
        name,
        is_primary: display.is_primary,
        cached_brightness,
        cached_contrast,
        level_error,
    }
}

fn build_monitor_list(previous: &[MonitorInfo]) -> Vec<MonitorInfo> {
    let displays = enumerate_gdi_displays();
    let identities = query_display_identities();
    let mut monitors = Vec::new();

    for display in displays {
        let identity = best_identity(&identities, &display.device);
        match read_physical_monitors(display.handle) {
            Ok(physical) => {
                for (physical_index, (description, levels)) in physical.into_iter().enumerate() {
                    monitors.push(describe_monitor(
                        previous,
                        &display,
                        identity,
                        physical_index as u32,
                        &description,
                        Some(levels),
                        None,
                    ));
                }
            }
            Err(err) => {
                log::warn!("Couldn't read controls for {}: {err}", display.device);
                monitors.push(describe_monitor(
                    previous,
                    &display,
                    identity,
                    0,
                    "",
                    None,
                    Some(err),
                ));
            }
        }
    }

    dedupe_monitors(monitors)
}

pub fn get_monitor_list_sync(previous: &[MonitorInfo]) -> Result<Vec<MonitorInfo>, String> {
    Ok(with_ddc(|| build_monitor_list(previous)))
}

/// Read brightness and contrast without borrowing a [`MonitorInfo`].
/// Callers must not hold the monitor list lock: this takes the DDC lock.
pub fn read_monitor_levels(
    display_handle: isize,
    physical_index: u32,
) -> Result<MonitorLevels, String> {
    with_ddc(|| {
        let physical = read_physical_monitors(display_handle)?;
        physical
            .get(physical_index as usize)
            .map(|(_, levels)| *levels)
            .ok_or_else(|| format!("Physical monitor index {physical_index} no longer exists"))
    })
}

/// Write brightness and contrast. Unchanged cached channels are not sent to the monitor.
pub fn apply_cached_levels(
    display_handle: isize,
    physical_index: u32,
    brightness: u32,
    contrast: u32,
    cached_brightness: Option<u32>,
    cached_contrast: Option<u32>,
) -> Result<MonitorLevels, String> {
    if brightness > 100 || contrast > 100 {
        return Err("Values must be between 0 and 100".to_string());
    }
    let brightness_changed = cached_brightness != Some(brightness);
    let contrast_changed = cached_contrast != Some(contrast);
    if !brightness_changed && !contrast_changed {
        return Ok(MonitorLevels {
            brightness,
            contrast,
        });
    }
    let apply = with_ddc(|| {
        set_monitor_settings_sync_internal(
            display_handle,
            physical_index,
            brightness,
            contrast,
            brightness_changed,
            contrast_changed,
        )
    })?;
    Ok(MonitorLevels {
        brightness: if apply.brightness_succeeded || !brightness_changed {
            brightness
        } else {
            cached_brightness.unwrap_or(brightness)
        },
        contrast: if apply.contrast_succeeded || !contrast_changed {
            contrast
        } else {
            cached_contrast.unwrap_or(contrast)
        },
    })
}

struct ApplyResult {
    brightness_succeeded: bool,
    contrast_succeeded: bool,
}

fn set_monitor_settings_sync_internal(
    monitor_handle: isize,
    physical_index: u32,
    brightness: u32,
    contrast: u32,
    apply_brightness: bool,
    apply_contrast: bool,
) -> Result<ApplyResult, String> {
    let ddc = get_ddc_functions().map_err(|err| format!("DDC/CI error: {err}"))?;
    let physical_monitors = retry_sessions("Open physical monitors", || {
        open_physical_monitors(monitor_handle)
    })?;

    let mut any_success = false;
    let mut brightness_success = false;
    let mut contrast_success = false;
    let mut last_error: Option<String> = None;

    let scale_to_range = |value: u32, min_v: u32, max_v: u32| -> u32 {
        if max_v <= min_v {
            return min_v;
        }
        let span = u64::from(max_v - min_v);
        let scaled = (u64::from(value) * span + 50) / 100;
        min_v + scaled as u32
    };

    let Some(pm) = physical_monitors.get(physical_index as usize) else {
        return Err(format!(
            "Physical monitor index {physical_index} no longer exists"
        ));
    };
    let handle = pm.handle;

    if apply_brightness {
        let mut min_b = 0u32;
        let mut cur_b = 0u32;
        let mut max_b = 0u32;
        if retry_bool("Read brightness before write", || unsafe {
            (ddc.get_monitor_brightness)(handle, &mut min_b, &mut cur_b, &mut max_b).as_bool()
        }) {
            let target_b = scale_to_range(brightness, min_b, max_b);
            if target_b != cur_b {
                if retry_bool("Set brightness", || unsafe {
                    (ddc.set_monitor_brightness)(handle, target_b).as_bool()
                }) {
                    any_success = true;
                    brightness_success = true;
                } else {
                    last_error = Some("Failed to set brightness".to_string());
                }
            } else {
                any_success = true;
                brightness_success = true;
            }
        } else {
            last_error = Some("Brightness not supported".to_string());
        }
    }

    if apply_contrast {
        let mut min_c = 0u32;
        let mut cur_c = 0u32;
        let mut max_c = 0u32;
        if retry_bool("Read contrast before write", || unsafe {
            (ddc.get_monitor_contrast)(handle, &mut min_c, &mut cur_c, &mut max_c).as_bool()
        }) {
            let target_c = scale_to_range(contrast, min_c, max_c);
            if target_c != cur_c {
                if retry_bool("Set contrast", || unsafe {
                    (ddc.set_monitor_contrast)(handle, target_c).as_bool()
                }) {
                    any_success = true;
                    contrast_success = true;
                } else {
                    last_error = Some("Failed to set contrast".to_string());
                }
            } else {
                any_success = true;
                contrast_success = true;
            }
        } else {
            last_error = Some("Contrast not supported".to_string());
        }
    }

    if any_success || (!apply_brightness && !apply_contrast) {
        Ok(ApplyResult {
            brightness_succeeded: brightness_success,
            contrast_succeeded: contrast_success,
        })
    } else {
        Err(last_error.unwrap_or_else(|| "DDC/CI operation failed".to_string()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn monitor_name_prefers_model_over_generic_probe() {
        assert_eq!(
            monitor_name("Generic PnP Monitor", "Dell P2222H", r"\\.\DISPLAY2"),
            "Dell P2222H"
        );
        assert_eq!(
            monitor_name("Dell P2222H (HDMI)", "Dell P2222H", r"\\.\DISPLAY2"),
            "Dell P2222H (HDMI)"
        );
    }

    #[test]
    fn monitor_id_follows_device_path() {
        let path = r"\\?\DISPLAY#DEL40C2#UID4352";
        let identity = DisplayIdentity {
            gdi_device: r"\\.\DISPLAY1".into(),
            friendly_name: "Generic PnP Monitor".into(),
            device_path: path.into(),
            edid_manufacturer: 0x10AC,
            edid_product: 0x40C2,
            connector_instance: 1,
        };
        assert_eq!(monitor_id(Some(&identity), r"\\.\DISPLAY2", 0), path);
        assert_eq!(
            monitor_id(Some(&identity), r"\\.\DISPLAY1", 1),
            format!("{path}#1")
        );

        let mut without_path = identity.clone();
        without_path.device_path.clear();
        assert_eq!(
            monitor_id(Some(&without_path), r"\\.\DISPLAY2", 0),
            "edid:10AC:40C2:1"
        );
        assert_eq!(monitor_id(None, r"\\.\DISPLAY2", 0), r"gdi:\\.\DISPLAY2");
    }

    #[test]
    fn percent_rounds_to_nearest_and_clamps() {
        assert_eq!(percent_from_range(50, 0, 100), 50);
        assert_eq!(percent_from_range(25, 0, 50), 50);
        assert_eq!(percent_from_range(0, 0, 0), 0);
        assert_eq!(percent_from_range(200, 0, 100), 100);
    }
}
