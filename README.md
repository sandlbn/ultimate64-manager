# Ultimate64 Manager

A cross-platform desktop application for managing Commodore 64 Ultimate, Ultimate 64 Elite and Ultimate-II+ devices. Browse files, stream content, play SID music, mount disk images, and configure device settings.

[![Buy Me a Coffee](https://img.shields.io/badge/Buy%20Me%20a%20Coffee-support-yellow.svg)](https://buymeacoffee.com/sandlbn)

![Ultimate64 Manager](screenshot.gif)

## Features
- **Dual-Pane File Browser** – Local and remote file browsing side by side
- **FTP File Transfer** – Upload/download files via FTP with multi-file selection
- **Remote Directory Browser** – Browse Ultimate64 filesystem without mounting disks
- **Disk Management** – Mount D64, D71, D81, G64, G71 images to Drive A/B
- **Run Programs** – Direct load and run for PRG and CRT files
- **Music Player** – Play SID and MOD files with playlist support
  - Shuffle and repeat modes
  - Subsong navigation for multi-tune SID files
  - Song length database support (HVSC Songlengths.md5)
  - True pause/resume (freezes C64)
  - Configurable default song duration
- **Video Streaming** – Real-time VIC video with audio
  - Fullscreen mode (double-click or Opt+F / Alt+F)
  - Screenshot capture to Pictures folder
  - Unicast and Multicast support
- **Audio Streaming** – SID audio output via UDP
- **Configuration Editor** – Edit Ultimate64 configuration settings
- **Machine Control** – Pause, Resume, Reset, Reboot, Power Off
- **Remote keyboard input for BASIC and menus**

## Keyboard Shortcuts

| Shortcut | Action |
|----------|--------|
| `Opt+F` / `Alt+F` | Toggle video fullscreen |
| `ESC` | Exit fullscreen |

## Song Length Database

The music player can use the HVSC Songlengths.md5 database for accurate song durations. 
You can download it from the Music Player tab or place it manually at:

- **Windows**: `%APPDATA%\ultimate64-manager\Songlengths.md5`
- **macOS**: `~/Library/Application Support/ultimate64-manager/Songlengths.md5`
- **Linux**: `~/.config/ultimate64-manager/Songlengths.md5`

## Screenshots

Screenshots are saved to:
- **Windows**: `Pictures\Ultimate64\`
- **macOS**: `~/Pictures/Ultimate64/`
- **Linux**: `~/Pictures/Ultimate64/`

## Building

### Prerequisites

- Rust 1.81+
- **Linux (audio support required for streaming):**
  
```bash
sudo apt-get update && sudo apt-get install -y libasound2-dev
```

- For macOS bundle:
```bash
cargo install cargo-bundle
```

### Build

```bash
# Clone
git clone https://github.com/sandlbn/ultimate64-manager.git
cd ultimate64-manager

# Build
cargo build --release

# macOS bundle
cargo bundle --release
```
 
# Enabling Video & Audio Streaming

## Prerequisites

- Ultimate64 and your computer connected via Ethernet (WiFi is not supported for streaming)
- Note your computer's IP address (e.g., `192.168.1.100`)

## Step 1: Configure Stream Destination

You can configure the stream destination either on the Ultimate64 directly or through the app.

### Option A: Using Ultimate64 Manager (recommended)

1. Open Ultimate64 Manager and connect to your device
2. Go to **Configuration Editor** tab
3. Select **Data Streams** from the categories list
4. Set **Stream VIC to** to your computer's IP address (e.g., `10.0.0.141:11000`)
5. Set **Stream Audio to** to the same IP with port 11001 (e.g., `10.0.0.141:11001`)
6. Click **Apply All** to save settings

### Option B: Using F2 Menu on Ultimate64 / Commodore 64 Ultimate

1. Press **F2** to enter the Configuration Menu
2. Navigate to **Data Streams** settings
3. Set the stream destinations to your computer's IP address
4. Save settings and exit

## Step 2: Start Ultimate64 Manager

1. Launch Ultimate64 Manager
2. Go to the **Streaming** tab
3. Select **Unicast** mode (direct IP connection)
4. Verify port is set to **11000** (default)
5. Enable **Audio** checkbox if desired (uses port 11001)
6. Click **START** to begin listening for the stream

## Step 3: Start Streaming on Ultimate64 / Commodore 64 Ultimate

1. Press **F5** to open the Action Menu
2. Navigate to **Streams**
3. Select **Start VIC Stream** — enter destination IP if prompted
4. Select **Start Audio Stream** for audio (optional)

The video should now appear in Ultimate64 Manager.

## Stopping the Stream

- **On Ultimate64**: F5 → Streams → Stop VIC Stream / Stop Audio Stream
- **In Ultimate64 Manager**: Click **STOP**

## Remote Keyboard

When video streaming is active, click the **⌨ Disabled** button to enable keyboard input. Your keystrokes will be sent directly to the C64.

**Supported:** BASIC programming, menu navigation, text adventures
**Not supported:** Games requiring held keys (uses keyboard buffer, not CIA matrix)

## Troubleshooting

| Issue | Solution |
|-------|----------|
| No video received | Verify IP address, check firewall allows UDP port 11000 |
| No audio | Ensure audio stream is started separately, check port 11001 |
| Stream not working over WiFi | Streaming requires Ethernet connection, WiFi is not supported |
| Windows firewall | Run: `netsh advfirewall firewall add rule name="Ultimate64 Stream" dir=in action=allow protocol=UDP localport=11000-11001` |

## Quick Start

1. Launch the application
2. Enter your Ultimate64 IP address
3. Click **Connect**
4. Browse files, stream video, play music, or control the machine

## Configuration

Settings are stored in:

- **Windows**: `%APPDATA%\ultimate64-manager\settings.json`
- **macOS**: `~/Library/Application Support/ultimate64-manager/settings.json`
- **Linux**: `~/.config/ultimate64-manager/settings.json`

## License

MIT License

## Acknowledgments

- [Ultimate64](https://github.com/GideonZ/1541ultimate) team
- [Ultimate64 Rust Library](https://github.com/mlund/ultimate64)
- [Iced](https://github.com/iced-rs/iced) GUI framework
