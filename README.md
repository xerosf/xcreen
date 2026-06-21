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
- **Direct Monitor Controls**: Adjust brightness and contrast with accessible native sliders
- **Windows Autostart**: Automatically start the application when Windows boots
- **Configurable Settings**: All settings stored in a config.json file
- **Monitor Support**: Works with all DDC/CI compatible external monitors
- **Native System Tray**: Lightweight tray integration
- **Monitor Detection**: Automatically detects and manages external monitors that support DDC/CI

## Installation

### From Release

1. Download the latest XCreen release archive from the [Releases](https://github.com/xerosf/XCreen/releases) page
2. Extract the complete archive to your desired location; XCreen's self-contained WinUI runtime files must remain beside `XCreen.exe`
3. Run `XCreen.exe`
4. Left-click the system tray icon to open the monitor controls, or right-click it for Open, Refresh, and Exit

## Configuration

The application uses a `config.json` file located in the same directory as the executable. This file is automatically created with default values when the app first runs.

### Configuration Options

```json
{
  "autostart_enabled": false,
  "last_brightness": 50,
  "monitors": [
    {
      "id": "display-specific-id",
      "display_device": "\\\\.\\DISPLAY1",
      "physical_index": 0,
      "name": "External Monitor"
    }
  ],
  "log_level": "warn"
}
```

## License

MIT License - see [LICENSE](LICENSE) file for details.
