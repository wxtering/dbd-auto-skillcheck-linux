//! Configuration loading from TOML.
//! File: ~/.config/dbd-auto-skillcheck-linux/config.toml
//! If missing, a default config is created automatically.

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Config {
    #[serde(default)]
    pub geometry: GeometryConfig,
    #[serde(default)]
    pub detection: DetectionConfig,
    #[serde(default)]
    pub colors: ColorConfig,
    #[serde(default)]
    pub timing: TimingConfig,
    #[serde(default)]
    pub input: InputConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GeometryConfig {
    pub circle_radius: u32,
    pub circle_center_x: u32,
    pub circle_center_y: u32,
    pub crop_size: u32,
}

impl Default for GeometryConfig {
    fn default() -> Self {
        Self {
            circle_radius: 65,
            circle_center_x: 960,
            circle_center_y: 530,
            crop_size: 300,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DetectionConfig {
    pub dark_value_threshold: f32,
    pub inner_enter: f32,
    pub inner_exit: f32,
    pub ring_boost: f32,
    pub ring_discount: f32,
    pub grey_v_min: f32,
    pub grey_v_max: f32,
    pub grey_s_max: f32,
}

impl Default for DetectionConfig {
    fn default() -> Self {
        Self {
            dark_value_threshold: 0.10,
            inner_enter: 0.75,
            inner_exit: 0.55,
            ring_boost: 0.70,
            ring_discount: 0.25,
            grey_v_min: 0.15,
            grey_v_max: 0.70,
            grey_s_max: 0.30,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ColorConfig {
    pub red_hue_min: f32,
    pub red_hue_max: f32,
    pub red_sat_min: f32,
    pub red_val_min: f32,
    pub white_sat_max: f32,
    pub white_val_min: f32,
}

impl Default for ColorConfig {
    fn default() -> Self {
        Self {
            red_hue_min: 15.0,
            red_hue_max: 345.0,
            red_sat_min: 0.6,
            red_val_min: 0.5,
            white_sat_max: 0.15,
            white_val_min: 0.85,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TimingConfig {
    pub speed_history_min: usize,
    pub latency_ms: f32,
    pub calibrating_samples: usize,
    pub active_miss: usize,
    pub calibrating_miss: usize,
}

impl Default for TimingConfig {
    fn default() -> Self {
        Self {
            speed_history_min: 8,
            latency_ms: 20.0,
            calibrating_samples: 3,
            active_miss: 3,
            calibrating_miss: 2,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InputConfig {
    pub device_name: String,
    pub vendor_id: u16,
    pub product_id: u16,
}

impl Default for InputConfig {
    fn default() -> Self {
        Self {
            device_name: "Logitech USB Keyboard".to_string(),
            vendor_id: 0x046d,
            product_id: 0xC52B,
        }
    }
}

fn get_config_folder() -> PathBuf {
    let base = dirs::config_dir().expect("cannot access config dir");
    let folder = base.join("dbd-auto-skillcheck-linux");
    std::fs::create_dir_all(&folder).expect("cannot create config folder");
    folder
}

/// Load config from disk. Creates a default one if missing.
pub fn get_config() -> Config {
    let path = get_config_folder().join("config.toml");
    if path.exists() {
        let text = std::fs::read_to_string(&path).expect("failed to read config file");
        let cfg: Config =
            toml::from_str(&text).expect("failed to parse config (check TOML syntax)");
        cfg
    } else {
        let cfg = Config::default();
        std::fs::write(&path, DEFAULT_CONFIG_TOML).expect("failed to write config file");
        println!("Created default config at {}", path.display());
        cfg
    }
}

// ── Default config TOML with comments ─────────────────────────────────────────
const DEFAULT_CONFIG_TOML: &str = r##"# Skillcheck circle detection geometry (resolution-dependent).
[geometry]
circle_center_x = 960
circle_center_y = 530
circle_radius = 65
# [probably you don't want to change this]
crop_size = 300

[colors]
# Red pointer: hue range (hue outside this band = red), min saturation, min brightness.
red_hue_min = 15.0
red_hue_max = 345.0
red_sat_min = 0.6
red_val_min = 0.5
# White (Great) zone: max saturation, min brightness.
white_sat_max = 0.15
white_val_min = 0.85

[timing]
# Click trigger offset (ms). Increase if late, decrease if early.
latency_ms = 20.0
# [probably you don't want to change this]
speed_history_min = 8
calibrating_samples = 3
active_miss = 3
calibrating_miss = 2

	[input]
device_name = "Logitech USB Keyboard"
vendor_id = 1133
product_id = 50475

# [probably you don't want to change this]
[detection]
dark_value_threshold = 0.1
inner_enter = 0.75
inner_exit = 0.55
ring_boost = 0.7
ring_discount = 0.25
grey_v_min = 0.15
grey_v_max = 0.7
grey_s_max = 0.3
"##;
