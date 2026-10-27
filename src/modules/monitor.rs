use std::thread;
use std::time::Duration;
use windows::Win32::{
    Devices::Display::{
        DISPLAYCONFIG_DEVICE_INFO_GET_SOURCE_NAME, DISPLAYCONFIG_DEVICE_INFO_GET_TARGET_NAME,
        DISPLAYCONFIG_DEVICE_INFO_HEADER, DISPLAYCONFIG_MODE_INFO, DISPLAYCONFIG_PATH_INFO,
        DISPLAYCONFIG_SOURCE_DEVICE_NAME, DISPLAYCONFIG_TARGET_DEVICE_NAME,
        DestroyPhysicalMonitors, DisplayConfigGetDeviceInfo, GetDisplayConfigBufferSizes,
        GetMonitorBrightness, GetMonitorContrast, GetNumberOfPhysicalMonitorsFromHMONITOR,
        GetPhysicalMonitorsFromHMONITOR, PHYSICAL_MONITOR, QDC_ONLY_ACTIVE_PATHS,
        QueryDisplayConfig, SetMonitorBrightness, SetMonitorContrast,
    },
    Foundation::{ERROR_INSUFFICIENT_BUFFER, ERROR_SUCCESS, HANDLE, LPARAM, RECT},
    Graphics::Gdi::{
        DISPLAYCONFIG_PATH_ACTIVE, EnumDisplayMonitors, GetMonitorInfoW, HDC, HMONITOR,
        MONITORINFOEXW,
    },
    UI::WindowsAndMessaging::MONITORINFOF_PRIMARY,
};
use windows::core::BOOL;

struct PhysicalMonitorList(Vec<PHYSICAL_MONITOR>);

impl Drop for PhysicalMonitorList {
    fn drop(&mut self) {
        // SAFETY: This list exclusively owns the handles returned by physical monitor
        // enumeration. No handle escapes its lifetime.
        unsafe {
            let _ = DestroyPhysicalMonitors(&self.0);
        }
    }
}

fn open_physical_monitors(monitor_handle: isize) -> Result<PhysicalMonitorList, String> {
    // SAFETY: The HMONITOR comes from EnumDisplayMonitors. The count pointer and
    // initialized array remain valid for the synchronous API calls.
    unsafe {
        let mut count = 0;
        GetNumberOfPhysicalMonitorsFromHMONITOR(HMONITOR(monitor_handle as _), &mut count)
            .map_err(|err| format!("Physical monitor count failed: {err}"))?;
        let mut monitors = vec![PHYSICAL_MONITOR::default(); count as usize];
        GetPhysicalMonitorsFromHMONITOR(HMONITOR(monitor_handle as _), &mut monitors)
            .map_err(|err| format!("Physical monitor enumeration failed: {err}"))?;
        Ok(PhysicalMonitorList(monitors))
    }
}

/// Bound transient DDC/CI retries while displays wake.
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
                tracing::warn!("{action} failed ({session}/{DDC_SESSIONS}): {last_error}");
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
            tracing::warn!("{action} did not succeed");
        } else {
            thread::sleep(DDC_RETRY_GAP);
        }
    }
    false
}

pub struct MonitorInfo {
    pub id: String,
    pub display_handle: isize,
    pub display_device: String,
    pub physical_index: u32,
    pub name: String,
    pub is_primary: bool,
    pub levels: Option<MonitorLevels>,
    pub level_error: Option<String>,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct MonitorLevels {
    pub brightness: Option<u32>,
    pub contrast: Option<u32>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LevelKind {
    Brightness,
    Contrast,
}

impl LevelKind {
    fn read(self, handle: HANDLE) -> Result<(u32, u32, u32), String> {
        let (mut min, mut current, mut max) = (0, 0, 0);
        // SAFETY: The handle belongs to a live PhysicalMonitorList and output
        // pointers reference initialized u32 values throughout the synchronous call.
        let success = retry_bool("Read monitor control", || unsafe {
            (match self {
                Self::Brightness => GetMonitorBrightness(handle, &mut min, &mut current, &mut max),
                Self::Contrast => GetMonitorContrast(handle, &mut min, &mut current, &mut max),
            }) != 0
        });
        if success && max > min {
            Ok((min, current, max))
        } else {
            Err(format!("{self:?} is unavailable"))
        }
    }

    fn write(self, handle: HANDLE, value: u32) -> Result<u32, String> {
        let (min, current, max) = self.read(handle)?;
        let target = min + ((u64::from(value) * u64::from(max - min) + 50) / 100) as u32;
        // SAFETY: The handle is owned by a live PhysicalMonitorList. The target
        // is within the range returned by this monitor for the requested control.
        if target == current
            // SAFETY: The live physical monitor owns this handle; target is in its reported range.
            || retry_bool("Write monitor control", || unsafe {
                (match self {
                    Self::Brightness => SetMonitorBrightness(handle, target),
                    Self::Contrast => SetMonitorContrast(handle, target),
                }) != 0
            })
        {
            Ok(percent_from_range(target, min, max))
        } else {
            Err(format!("Failed to set {self:?}"))
        }
    }
}

fn read_levels_from_handle(handle: HANDLE) -> Result<MonitorLevels, String> {
    let brightness = LevelKind::Brightness
        .read(handle)
        .ok()
        .map(|(min, current, max)| percent_from_range(current, min, max));
    let contrast = LevelKind::Contrast
        .read(handle)
        .ok()
        .map(|(min, current, max)| percent_from_range(current, min, max));
    if brightness.is_none() && contrast.is_none() {
        Err("DDC/CI brightness and contrast are unavailable".into())
    } else {
        Ok(MonitorLevels {
            brightness,
            contrast,
        })
    }
}

fn validate_display(monitor: &MonitorInfo) -> Result<(), String> {
    let mut info = MONITORINFOEXW::default();
    info.monitorInfo.cbSize = std::mem::size_of::<MONITORINFOEXW>() as u32;
    // SAFETY: info has the API-required size and a live writable buffer. Windows
    // rejects an HMONITOR that became invalid after a display configuration change.
    if unsafe {
        GetMonitorInfoW(HMONITOR(monitor.display_handle as _), &mut info.monitorInfo).as_bool()
    } && wide_to_string(&info.szDevice).eq_ignore_ascii_case(&monitor.display_device)
    {
        Ok(())
    } else {
        Err("The selected display is no longer connected; refresh monitors".into())
    }
}

pub fn read_monitor_levels(monitor: &MonitorInfo) -> Result<MonitorLevels, String> {
    validate_display(monitor)?;
    retry_sessions("Read monitor controls", || {
        let physical = open_physical_monitors(monitor.display_handle)?;
        let pm = physical
            .0
            .get(monitor.physical_index as usize)
            .ok_or_else(|| "Physical monitor is no longer connected".to_string())?;
        read_levels_from_handle(pm.hPhysicalMonitor)
    })
}

/// Updates only requested channels. Successful channels remain cached if another fails.
pub fn apply_levels(monitor: &mut MonitorInfo, requested: MonitorLevels) -> Result<(), String> {
    if requested.brightness.is_some_and(|v| v > 100) || requested.contrast.is_some_and(|v| v > 100)
    {
        return Err("Values must be between 0 and 100".into());
    }
    validate_display(monitor)?;
    let physical = retry_sessions("Open monitor controls", || {
        open_physical_monitors(monitor.display_handle)
    })?;
    let pm = physical
        .0
        .get(monitor.physical_index as usize)
        .ok_or_else(|| "Physical monitor is no longer connected".to_string())?;
    let levels = monitor.levels.get_or_insert_with(MonitorLevels::default);
    apply_requested(levels, requested, |kind, value| {
        kind.write(pm.hPhysicalMonitor, value)
    })
}

fn apply_requested(
    levels: &mut MonitorLevels,
    requested: MonitorLevels,
    mut write: impl FnMut(LevelKind, u32) -> Result<u32, String>,
) -> Result<(), String> {
    let mut errors = Vec::new();
    for (kind, value, cached) in [
        (
            LevelKind::Brightness,
            requested.brightness,
            &mut levels.brightness,
        ),
        (
            LevelKind::Contrast,
            requested.contrast,
            &mut levels.contrast,
        ),
    ] {
        let Some(value) = value else {
            continue;
        };
        match write(kind, value) {
            Ok(actual) => *cached = Some(actual),
            Err(err) => {
                *cached = None;
                errors.push(err);
            }
        }
    }
    if errors.is_empty() {
        Ok(())
    } else {
        Err(errors.join("; "))
    }
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

struct GdiDisplay {
    handle: isize,
    device: String,
    is_primary: bool,
}

struct DisplayIdentity {
    gdi_device: String,
    friendly_name: String,
    device_path: String,
}

fn enumerate_gdi_displays() -> Result<Vec<GdiDisplay>, String> {
    let mut displays = Vec::new();

    unsafe extern "system" fn enum_monitor_proc(
        hmonitor: HMONITOR,
        _hdc: HDC,
        _rect: *mut RECT,
        lparam: LPARAM,
    ) -> BOOL {
        // SAFETY: Enumeration is synchronous and receives the Vec pointer supplied below.
        let displays = unsafe { &mut *(lparam.0 as *mut Vec<GdiDisplay>) };
        let mut info = MONITORINFOEXW {
            monitorInfo: Default::default(),
            szDevice: [0; 32],
        };
        info.monitorInfo.cbSize = std::mem::size_of::<MONITORINFOEXW>() as u32;
        // SAFETY: info has the required size and the callback supplies an HMONITOR.
        if unsafe { GetMonitorInfoW(hmonitor, &mut info.monitorInfo as *mut _ as *mut _).as_bool() }
        {
            displays.push(GdiDisplay {
                handle: hmonitor.0 as isize,
                device: wide_to_string(&info.szDevice),
                is_primary: (info.monitorInfo.dwFlags & MONITORINFOF_PRIMARY) != 0,
            });
        }
        BOOL::from(true)
    }

    // SAFETY: displays lives until synchronous enumeration completes.
    unsafe {
        EnumDisplayMonitors(
            None,
            None,
            Some(enum_monitor_proc),
            LPARAM(&mut displays as *mut _ as isize),
        )
        .ok()
        .map_err(|err| format!("Display enumeration failed: {err}"))?;
    }
    Ok(displays)
}

fn query_display_identities() -> Vec<DisplayIdentity> {
    match query_display_identities_inner() {
        Ok(identities) => identities,
        Err(err) => {
            tracing::warn!(
                "Display configuration query failed; monitor names may be generic: {err}"
            );
            Vec::new()
        }
    }
}

fn query_display_identities_inner() -> Result<Vec<DisplayIdentity>, String> {
    // SAFETY: Buffers have the initialized sizes reported by Windows, and each
    // device-info request below supplies its required header size and path IDs.
    unsafe {
        let mut paths = Vec::new();
        for attempt in 0..3 {
            let (mut path_count, mut mode_count) = (0, 0);
            let status = GetDisplayConfigBufferSizes(
                QDC_ONLY_ACTIVE_PATHS,
                &mut path_count,
                &mut mode_count,
            );
            if status != ERROR_SUCCESS {
                return Err(format!("Display buffer sizing failed ({})", status.0));
            }
            paths = vec![DISPLAYCONFIG_PATH_INFO::default(); path_count as usize];
            let mut modes = vec![DISPLAYCONFIG_MODE_INFO::default(); mode_count as usize];
            let status = QueryDisplayConfig(
                QDC_ONLY_ACTIVE_PATHS,
                &mut path_count,
                paths.as_mut_ptr(),
                &mut mode_count,
                modes.as_mut_ptr(),
                None,
            );
            if status == ERROR_INSUFFICIENT_BUFFER && attempt < 2 {
                continue;
            }
            if status != ERROR_SUCCESS {
                return Err(format!("Display configuration query failed ({})", status.0));
            }
            paths.truncate(path_count as usize);
            break;
        }

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
            });
        }
        Ok(identities)
    }
}

fn best_identity<'a>(
    identities: &'a [DisplayIdentity],
    gdi_device: &str,
) -> Option<&'a DisplayIdentity> {
    let mut matching = identities
        .iter()
        .filter(|identity| identity.gdi_device.eq_ignore_ascii_case(gdi_device));
    let identity = matching.next()?;
    // Clone-mode targets share a GDI source; its physical-monitor order is not
    // specified to match the display configuration path order.
    if matching.next().is_some() {
        None
    } else {
        Some(identity)
    }
}

fn monitor_id(identity: Option<&DisplayIdentity>, gdi_device: &str, physical_index: u32) -> String {
    if let Some(identity) = identity
        && !identity.device_path.trim().is_empty()
    {
        return indexed_id(identity.device_path.trim(), physical_index);
    }
    indexed_id(&format!("gdi:{gdi_device}"), physical_index)
}

pub fn get_monitor_list_sync() -> Result<Vec<MonitorInfo>, String> {
    let identities = query_display_identities();
    let mut monitors = Vec::new();
    for display in enumerate_gdi_displays()? {
        let identity = best_identity(&identities, &display.device);
        let physical = match retry_sessions("Discover physical monitors", || {
            open_physical_monitors(display.handle)
        }) {
            Ok(physical) => physical,
            Err(err) => {
                monitors.push(MonitorInfo {
                    id: monitor_id(identity, &display.device, 0),
                    display_handle: display.handle,
                    display_device: display.device.clone(),
                    physical_index: 0,
                    name: monitor_name(
                        "",
                        identity
                            .map(|item| item.friendly_name.as_str())
                            .unwrap_or(""),
                        &display.device,
                    ),
                    is_primary: display.is_primary,
                    levels: None,
                    level_error: Some(err),
                });
                continue;
            }
        };
        monitors.extend(describe_physical_monitors(
            &display,
            identity,
            &physical.0,
            read_levels_from_handle,
        ));
    }
    monitors.sort_by_key(|monitor| !monitor.is_primary);
    Ok(monitors)
}

fn describe_physical_monitors(
    display: &GdiDisplay,
    identity: Option<&DisplayIdentity>,
    physical: &[PHYSICAL_MONITOR],
    mut read_levels: impl FnMut(HANDLE) -> Result<MonitorLevels, String>,
) -> Vec<MonitorInfo> {
    let mut monitors = Vec::with_capacity(physical.len());
    for (index, pm) in physical.iter().enumerate() {
        let description = pm.szPhysicalMonitorDescription;
        let (levels, level_error) = match read_levels(pm.hPhysicalMonitor) {
            Ok(levels) => (Some(levels), None),
            Err(err) => (None, Some(err)),
        };
        monitors.push(MonitorInfo {
            id: monitor_id(identity, &display.device, index as u32),
            display_handle: display.handle,
            display_device: display.device.clone(),
            physical_index: index as u32,
            name: monitor_name(
                &wide_to_string(&description),
                identity
                    .map(|item| item.friendly_name.as_str())
                    .unwrap_or(""),
                &display.device,
            ),
            is_primary: display.is_primary,
            levels,
            level_error,
        });
    }
    monitors
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn partial_write_reports_failure_without_fabricating_a_successful_value() {
        let mut levels = MonitorLevels {
            brightness: Some(40),
            contrast: Some(50),
        };
        let requested = MonitorLevels {
            brightness: Some(75),
            contrast: Some(60),
        };
        let result = apply_requested(&mut levels, requested, |kind, _| match kind {
            LevelKind::Brightness => Ok(74),
            LevelKind::Contrast => Err("Disconnected during contrast write".into()),
        });
        assert_eq!(result, Err("Disconnected during contrast write".into()));
        assert_eq!(levels.brightness, Some(74));
        assert_eq!(levels.contrast, None);
    }

    #[test]
    fn changing_brightness_never_writes_contrast() {
        let mut levels = MonitorLevels {
            brightness: Some(40),
            contrast: None,
        };
        let mut written = Vec::new();
        apply_requested(
            &mut levels,
            MonitorLevels {
                brightness: Some(75),
                contrast: None,
            },
            |kind, value| {
                written.push(kind);
                Ok(value)
            },
        )
        .expect("brightness write succeeds without contrast support");
        assert_eq!(written, [LevelKind::Brightness]);
        assert_eq!(levels.contrast, None);
    }

    #[test]
    fn failed_probe_keeps_later_physical_indices_and_brightness_only_support() {
        let display = GdiDisplay {
            handle: 0,
            device: "DISPLAY1".into(),
            is_primary: true,
        };
        let physical = [
            PHYSICAL_MONITOR {
                hPhysicalMonitor: HANDLE(1usize as _),
                ..Default::default()
            },
            PHYSICAL_MONITOR {
                hPhysicalMonitor: HANDLE(2usize as _),
                ..Default::default()
            },
        ];
        let monitors = describe_physical_monitors(&display, None, &physical, |handle| {
            if handle == HANDLE(1usize as _) {
                Err("Display is waking".into())
            } else {
                Ok(MonitorLevels {
                    brightness: Some(42),
                    contrast: None,
                })
            }
        });
        assert_eq!(monitors.len(), 2);
        assert_eq!(monitors[0].physical_index, 0);
        assert!(monitors[0].level_error.is_some());
        assert_eq!(monitors[1].physical_index, 1);
        assert_eq!(
            monitors[1].levels,
            Some(MonitorLevels {
                brightness: Some(42),
                contrast: None
            })
        );
        assert_ne!(monitors[0].id, monitors[1].id);
    }

    #[test]
    fn cloned_targets_do_not_borrow_each_others_identity() {
        let identities = [
            DisplayIdentity {
                gdi_device: "DISPLAY1".into(),
                friendly_name: "Dell".into(),
                device_path: "dell-path".into(),
            },
            DisplayIdentity {
                gdi_device: "DISPLAY1".into(),
                friendly_name: "LG".into(),
                device_path: "lg-path".into(),
            },
        ];
        assert!(best_identity(&identities, "display1").is_none());
        assert!(best_identity(&identities[..1], "display1").is_some());
        assert!(best_identity(&identities, "DISPLAY2").is_none());
    }

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
        };
        assert_eq!(monitor_id(Some(&identity), r"\\.\DISPLAY2", 0), path);
        assert_eq!(
            monitor_id(Some(&identity), r"\\.\DISPLAY1", 1),
            format!("{path}#1")
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
