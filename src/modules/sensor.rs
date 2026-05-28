use windows::Devices::Sensors::LightSensor;

/// Check whether Windows exposes a default ambient light sensor
pub fn has_light_sensor() -> bool {
    LightSensor::GetDefault().is_ok()
}

/// Read the ambient light sensor value in lux
pub fn get_light_sensor_lux() -> Result<f64, String> {
    let sensor =
        LightSensor::GetDefault().map_err(|e| format!("No light sensor available: {}", e))?;

    let reading = sensor
        .GetCurrentReading()
        .map_err(|e| format!("Failed to read sensor: {}", e))?;

    reading
        .IlluminanceInLux()
        .map(|lux| lux as f64)
        .map_err(|e| format!("Failed to get lux value: {}", e))
}
