pub trait InputEmulatorBackend {
    fn press_space(&mut self) -> Result<(), std::io::Error>;
}

#[cfg(target_os = "linux")]
pub mod linux;
#[cfg(target_os = "linux")]
pub use linux::LinuxKeyboardEmulator as KeyboardEmulator;

#[cfg(target_os = "windows")]
pub mod windows;
#[cfg(target_os = "windows")]
pub use windows::WindowsKeyboardEmulator as KeyboardEmulator;
