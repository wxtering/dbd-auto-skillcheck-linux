use evdev::uinput::VirtualDevice;
use evdev::{AttributeSet, BusType, EventType, InputEvent, InputId, KeyCode};
use std::thread;
use std::time::Duration;

pub struct KeyboardEmulator {
    device: VirtualDevice,
}

impl KeyboardEmulator {
    pub fn new(name: &str, vendor_id: u16, product_id: u16) -> Result<Self, std::io::Error> {
        let mut keys = AttributeSet::<KeyCode>::new();
        keys.insert(KeyCode::KEY_SPACE);

        // Spoof real device IDs to avoid uinput fingerprint detection.
        let id = InputId::new(BusType::BUS_USB, vendor_id, product_id, 0x0111);

        let device = VirtualDevice::builder()?
            .name(name)
            .input_id(id)
            .with_keys(&keys)?
            .build()?;

        Ok(Self { device })
    }

    pub fn press_space(&mut self) -> Result<(), std::io::Error> {
        // emit() in evdev 0.13.2 automatically appends SYN_REPORT.
        self.device.emit(&[InputEvent::new(
            EventType::KEY.0,
            KeyCode::KEY_SPACE.code(),
            1,
        )])?;

        // Randomized hold time (10-50ms) to avoid detection via timing analysis.
        // frames (has_clicked = true).
        let hold_ms = 10 + rand::random::<u64>() % 41;
        thread::sleep(Duration::from_millis(hold_ms));

        self.device.emit(&[InputEvent::new(
            EventType::KEY.0,
            KeyCode::KEY_SPACE.code(),
            0,
        )])?;

        Ok(())
    }
}
