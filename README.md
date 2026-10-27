# XCreen

**Native Rust External Monitor Brightness Control**

<p align="center">
  <img src="src/icons/icon.png" alt="XCreen Logo" width="150" height="150" />
</p>

<p align="center">
  <strong>Adaptive brightness and contrast adjustment for external monitors using DDC/CI</strong>
</p>

A lightweight, modern Windows application built with **Rust** and **WinUI 3** for automatic and manual brightness control of external monitors.

## Features

- **Modern WinUI 3 Interface**: Built with native Windows 11 design principles and fluent controls using Rust bindings.
- **Ambient Light Brightness**: Automatically adapt monitor brightness and contrast based on your ambient light sensor readings.
- **Direct Monitor Controls**: Fine-tune brightness and contrast using responsive native sliders.
- **Windows Autostart**: Seamlessly launch the application on system boot.
- **Monitor Support**: Out-of-the-box compatibility with all DDC/CI-compliant external monitors.
- **Native System Tray**: Sits quietly in the system tray, keeping your workspace clean.

## Installation

### Using the Installer (Recommended)

1. Head over to the [Releases](https://github.com/xerosf/XCreen/releases) page and download the latest `XCreen-Setup-X.X.X.exe`.
2. Run the installer to automatically configure the application, place shortcuts, and handle the WinUI 3 dependencies.
3. Launch XCreen from your Desktop or Start Menu.
4. Left-click the system tray icon to pull up quick monitor controls, or right-click it to access the Settings panel, trigger a monitor refresh, or exit.

## Configuration

While the app features a fully native in-app settings menu, all preferences are backed by a `config.json` file stored locally next to the executable. The file is automatically generated on the first launch.

### Configuration Options

```json
{
  "autostart_enabled": false,
  "log_level": "warn"
}
```

Monitor discovery runs in the background at startup and after display changes or resume.
Opening the flyout reads the selected monitor; **Refresh Monitors** rescans all displays.
Brightness and contrast are enabled independently according to monitor support. DDC/CI
must be enabled in the monitor's own settings. Internal laptop panels usually do not
support DDC/CI.

Legacy monitor lists, `last_brightness`, and brightness profiles are ignored. Monitor
settings are read from the hardware rather than restored from the configuration file.
Configuration writes replace the file atomically. `XCreen.log`, next to the executable,
is capped at 1 MiB; it resets when the next entry would exceed that limit.

## Development

Build on Windows with Rust 1.95 or newer, the MSVC build tools, and the Windows SDK.
Install Rust's `rustfmt` and `clippy` components. Bun runs the repository tasks without
any JavaScript dependencies:

```powershell
bun fmt
bun verify
cargo build --release
```

Verification covers formatting, Clippy, and regression tests. Hardware checks should
also cover startup with sleeping monitors, hot-plugging, resume, brightness-only
monitors, and flyout placement on displays with different DPI and negative coordinates.
