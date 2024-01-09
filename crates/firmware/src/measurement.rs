#[derive(Debug, Copy, Clone)]
pub enum Measurement {
    Temperature(f32),
    Humidity(f32),
    Co2(u16),
}
