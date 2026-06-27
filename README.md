# DBD Auto-SkillCheck Linux

A simple and highly optimized auto-skillcheck bot for Dead by Daylight on Linux.

> ⚠️ **Important Warning**: This bot relies on PipeWire DMA-BUF frame negotiation, which is highly compositor-dependent. It has only been tested and is guaranteed to work out-of-the-box on the **Niri** compositor. On other compositors (like GNOME/Mutter or KDE/KWin), it may fallback to SHM (Shared Memory) streams and fail to run, or require additional configuration. 

Unlike traditional screenshot-based bots, this project imports PipeWire DMA-BUF frames directly into Vulkan, avoiding full-frame CPU copies. It performs GPU-side cropping and frame extraction, resulting in minimal CPU overhead and low latency.

## How It Works

1. **Screencast Capture (PipeWire)**
   * The compositor shares the game window framebuffer as a DMA-BUF file descriptor.
   
2. **GPU Processing (Vulkan)**
   * The DMA-BUF fd is imported directly into Vulkan as a GPU texture (`VkImage`).
   * A 300x300 target region (centered on the skillcheck widget) is cropped and copied to host-visible memory (`VkBuffer`), resolving the tiled GPU image layout into a linear host-visible buffer on the GPU.

3. **Color and Target Detection (CPU / HSV)**
   * The CPU reads the 300x300 buffer.
   * Checks if the dark inner circle of the skillcheck widget is active.
   * Sweeps a 360-degree circle to detect the white success zone and the red needle using HSV color ranges.

4. **Prediction and Trigger (Least-Squares)**
   * Accumulates needle positions over frames and estimates its angular velocity using least-squares linear regression.

5. **Key Injection (uinput / evdev)**
   * Injects keypress events directly into the kernel input stack via `/dev/uinput`, acting as a virtual hardware keyboard.

## Features
* **GPU-Assisted Crop**: Extracts a 300x300 pixel region directly on the GPU, avoiding full-frame CPU copies and minimizing latency.
* **Speed Prediction**: Estimates pointer speed so that should work with any skillcheck speed up perks.
* **Evdev Input Injection**: Emulates keypresses using `/dev/uinput`, appearing as a standard input device.

## Requirements
* **Rust toolchain** (cargo, rustc 1.75+ or newer)
* Wayland compositor supporting DMA-BUF screencast sharing (tested and works out-of-the-box on **Niri** and **KDE Plasma 6**; other compositors must negotiate DMA-BUF stream capture instead of fallback SHM).
* PipeWire (including headers/development libraries)
* **Clang** (required by `bindgen` to generate Rust bindings for PipeWire/SPA headers during build)
* **pkg-config** (required to locate PipeWire libraries)
* Vulkan 1.2+
* Linux with `/dev/uinput` support

## Tested Environment
* **OS**: Arch Linux
* **Display Server**: Wayland (Niri compositor)
* **Resolution**: 2560x1440 (QHD)
* **PipeWire**: v1.6.6
* **NVIDIA Driver**: v610.43.02
* **Reshade**: Tested without Reshade, but should work fine as long as colors are not overly distorted/brightened.

## Capture Source
For best results, share the game window when prompted by the Wayland screencast portal.

## Setup uinput Permissions

To run the bot without root permissions, your user needs write access to `/dev/uinput`.

First, check if your system already has a udev rule for uinput:
```bash
ls /lib/udev/rules.d/*uinput*
```
If you see a result (e.g. `80-uinput.rules` or `60-steam-input.rules`), the rule already exists — skip to step 2.

If not, create one at `/etc/udev/rules.d/99-uinput.rules`:
```bash
echo 'KERNEL=="uinput", GROUP="input", MODE="0660"' | sudo tee /etc/udev/rules.d/99-uinput.rules
```

Then:

1. Add your user to the `input` group:
   ```bash
   sudo usermod -aG input $USER
   ```
2. Reload udev rules and trigger uinput:
   ```bash
   sudo udevadm control --reload-rules && sudo udevadm trigger --action=add /dev/uinput
   ```
3. Log out and back in (or reboot) for group changes to take effect.

## Usage

1. Build the binary in release mode:
   ```bash
   cargo build --release
   ```
2. Run the application:
   ```bash
   ./target/release/dbd-auto-skillcheck-linux
   ```
   *(Wayland will prompt you to share your screen).*

Optionally, you can copy the compiled binary to your local path for global execution:
```bash
cp target/release/dbd-auto-skillcheck-linux ~/.local/bin/
```

## Configuration

A default configuration file is automatically created at:
`~/.config/dbd-auto-skillcheck-linux/config.toml`

* `circle_center_x` / `circle_center_y` — coordinates of the skillcheck widget (default is set for 2560x1440).
* `latency_ms` — input lag compensation (default 20.0 ms). **To click earlier, increase this value; to click later, decrease it** (since the bot triggers when `time_to_go <= latency_ms`).

### Tuning Parameters
To adapt the bot to your resolution (fhd and qhd should work fine) or reshade:
1. Take a screenshot during a skillcheck.
2. Load the screenshot into **GIMP** (or any image editor).
3. Use the **Color Picker** tool to find the pixel coordinates of the skillcheck circle's center, then update `circle_center_x` and `circle_center_y` in the configuration.

> **Note**: This bot processes colors in **HSV** (Hue, Saturation, Value) space instead of RGB. HSV color spaces are much more robust and work significantly better with **Reshade** shaders or custom in-game overlays. Tested without Reshade — Reshade should work fine as long as it doesn't significantly alter the HSV values of the skillcheck widget's core colors (white zone, red pointer).
> 
> ❄️ **Bright / Snow Maps (e.g., Ormond)**: Since the widget detection is based on circular HSV color thresholding (looking for a dark inner circle and the white zone), very bright/white backgrounds (like looking directly at snow, bright lights, or fog) might wash out or overlap with the widget's HSV thresholds. In such cases, the skillcheck might not be detected. To resolve this, you can adjust the `grey_v_min`/`grey_v_max` and `white_val_min` HSV thresholds in the configuration.

## TODO

- [ ] **Wiggle Mode** — auto-click during wiggle skillchecks.
- [ ] **Perk Support** — handle Bardic Inspiration / Onryo
- [ ] **Auto Focus** — when the bot detects a skillcheck, move the mouse cursor / focus to the game window
- [ ] **Improve Detection Stability** — reduce flicker on widget borders (inner/ring mask hysteresis + smoothing).
