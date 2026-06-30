use std::thread;
use std::time::Duration;
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
    INPUT, INPUT_KEYBOARD, KEYBDINPUT, KEYEVENTF_KEYUP, KEYEVENTF_SCANCODE, SendInput,
};

pub struct WindowsKeyboardEmulator;

impl WindowsKeyboardEmulator {
    pub fn new(_name: &str, _vendor_id: u16, _product_id: u16) -> Result<Self, std::io::Error> {
        Ok(Self)
    }
}

impl crate::input::InputEmulatorBackend for WindowsKeyboardEmulator {
    fn press_space(&mut self) -> Result<(), std::io::Error> {
        // Spacebar scan code on Windows is 0x39
        let space_scan_code = 0x39;

        unsafe {
            let mut inputs: [INPUT; 2] = std::mem::zeroed();

            // Key Down (using Scan Code for DirectInput games like Unreal Engine)
            inputs[0].r#type = INPUT_KEYBOARD;
            inputs[0].Anonymous.ki = KEYBDINPUT {
                wVk: 0,
                wScan: space_scan_code,
                dwFlags: KEYEVENTF_SCANCODE,
                time: 0,
                dwExtraInfo: 0,
            };

            // Key Up (using Scan Code + Release Flag)
            inputs[1].r#type = INPUT_KEYBOARD;
            inputs[1].Anonymous.ki = KEYBDINPUT {
                wVk: 0,
                wScan: space_scan_code,
                dwFlags: KEYEVENTF_SCANCODE | KEYEVENTF_KEYUP,
                time: 0,
                dwExtraInfo: 0,
            };

            SendInput(2, inputs.as_ptr(), std::mem::size_of::<INPUT>() as i32);
        }

        // Randomized hold time (10-50ms)
        let hold_ms = 10 + rand::random::<u64>() % 41;
        thread::sleep(Duration::from_millis(hold_ms));

        Ok(())
    }
}
