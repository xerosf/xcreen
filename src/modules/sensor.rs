use windows::{
    Devices::Sensors::LightSensor,
};

pub fn get_light_sensor_lux_sync() -> Result<f64, String> {
    let sensor = LightSensor::GetDefault()
        .map_err(|e| format!("Failed to get light sensor: {}", e))?;
    
    let reading = sensor.GetCurrentReading()
        .map_err(|e| format!("Failed to get sensor reading: {}", e))?;

    let lux = reading.IlluminanceInLux()
        .map_err(|e| format!("Failed to get illuminance value: {}", e))?;

    Ok(lux as f64)
}