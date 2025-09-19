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
- **Manual Brightness Profiles**: Set predefined brightness levels (Dim, Normal, Max, etc.)
- **Windows Autostart**: Automatically start the application when Windows boots
- **Configurable Settings**: All settings stored in a config.json file
- **Monitor Support**: Works with all DDC/CI compatible external monitors
- **Native System Tray**: Lightweight tray integration
- **Monitor Detection**: Automatically detects and manages external monitors that support DDC/CI

## Installation

### From Release

1. Download the latest `xcreen.exe` from the [Releases](https://github.com/xerosf/xcreen/releases) page
2. Place the executable in your desired location
3. Run `xcreen.exe`
4. Right-click the system tray icon to access features

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

## License

MIT License - see [LICENSE](LICENSE) file for details.