# XCreen

**Pure Rust External Monitor Brightness Control**

<p align="center">
  <img src="src/icons/icon.png" alt="XCreen Logo" width="150" height="150" />
</p>

<p align="center">
  <strong>Adaptive brightness and contrast adjustment for external monitors using DDC/CI</strong>
</p>

A lightweight, native Windows application for automatic brightness control of external monitors using ambient light sensors.

## Features

- **Ambient Light Brightness**: Manually set monitor brightness and contrast based on current ambient light sensor reading
- **Manual Brightness Profiles**: Set predefined brightness levels (Night, Dim, Normal, Bright, Outdoor)
- **Hardware Protection**: Prevents excessive EEPROM writes to external monitors
- **Windows Autostart**: Automatically start the application when Windows boots
- **Configurable Settings**: All settings stored in a config.json file for easy customization
- **Monitor Support**: Works with DDC/CI compatible external monitors
- **Native System Tray**: Lightweight tray integration
- **Monitor Detection**: Automatically detects and manages external monitors that support DDC/CI

## Performance

- **Binary Size**: ~900KB
- **Memory Usage**: ~4MB RAM
- **Native Performance**: Pure Rust with direct Windows API calls

## Requirements

- Windows 10/11
- DDC/CI compatible external monitors
- Accessible ambient light sensor (built-in or external)

## Installation

### From Release

1. Download the latest `xcreen.exe` from the [Releases](https://github.com/xerosf/xcreen/releases) page
2. Place the executable in your desired location
3. Run `xcreen.exe`
4. Right-click the system tray icon to access features

### From Source

```bash
git clone https://github.com/xerosf/xcreen.git
cd xcreen
cargo build --release --target x86_64-pc-windows-msvc
```

## Configuration

The application uses a `config.json` file located in the same directory as the executable. This file is automatically created with default values when the app first runs.

### Configuration Options

```json
{
  "autostart_enabled": false,
  "last_brightness": 50,
  "brightness_profiles": [
    {
      "name": "Dim",
      "brightness": 15,
      "contrast": 30
    },
    {
      "name": "Normal",
      "brightness": 55,
      "contrast": 75
    },
    {
      "name": "Max",
      "brightness": 100,
      "contrast": 100
    }
  ],
  "log_level": "warn"
}
```

## Usage

### System Tray Menu

When the tray icon is visible, right-click to access:

- **Set from Ambient Light**: Set brightness based on current ambient light sensor reading
- **Enable/Disable Autostart**: Toggle Windows startup behavior
- **Set Brightness**: Set brightness and contrast from configurable presets
- **Refresh Monitors**: Refresh the list of compatible monitors
- **Exit**: Close the application

### Ambient Light Brightness

The application provides an on-demand brightness adjustment feature that:

1. Reads current ambient light sensor data once when triggered
2. Calculates optimal brightness and contrast based on current lighting conditions:
   - **< 10 lux**: 20% brightness, 40% contrast (very dark)
   - **10-50 lux**: 30% brightness, 45% contrast (dark)
   - **50-100 lux**: 40% brightness, 55% contrast (dim)
   - **100-200 lux**: 50% brightness, 65% contrast (indoor)
   - **200-500 lux**: 65% brightness, 75% contrast (bright indoor)
   - **500-1000 lux**: 80% brightness, 85% contrast (very bright)
   - **> 1000 lux**: 100% brightness, 95% contrast (outdoor)
3. Applies settings to all compatible external monitors
4. **Hardware Protection**: Prevents excessive EEPROM writes by avoiding continuous writes

## Technical Details

### Architecture
- **Pure Rust**: No web framework dependencies
- **Native Windows APIs**: Direct DDC/CI and sensor access
- **Async/Multithreaded**: Tokio for async operations, background threads for continuous monitoring
- **Minimal Dependencies**: Only essential crates for Windows integration

### Key Dependencies
- `tray-icon` - Native system tray integration
- `winit` - Cross-platform window and event handling
- `windows` - Windows API bindings
- `tokio` - Async runtime
- `winreg` - Windows registry manipulation for autostart

## Troubleshooting

### Monitors Not Detected
- Ensure your monitors support DDC/CI
- Try different cables (some HDMI cables don't support DDC/CI)
- Run "Refresh Monitors" from tray menu

### Auto-Brightness Not Working
- Verify your device has an ambient light sensor
- Check Windows privacy settings for sensor access

## Contributing

Contributions are welcome! Please feel free to submit a Pull Request.

## License

MIT License - see [LICENSE](LICENSE) file for details.