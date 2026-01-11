// Telnet Menu Navigator for Ultimate64
// Controls streaming settings via telnet menu navigation
// Supports both Ultimate64 (F1 menu) and Elite II (F5 menu) boards

use std::io::{Read, Write};
use std::net::TcpStream;
use std::time::Duration;

/// Default telnet port
const TELNET_PORT: u16 = 23;

/// Connection timeout in seconds
const CONNECT_TIMEOUT_SECS: u64 = 5;

/// Read/write timeout in seconds
const IO_TIMEOUT_SECS: u64 = 2;

// ANSI escape sequences for menu navigation (public for UI use)
pub const F1_KEY_VT100: &[u8] = b"\x1bOP";
pub const F1_KEY_XTERM: &[u8] = b"\x1b[11~";
pub const F1_KEY_LINUX: &[u8] = b"\x1b[[A";
pub const F5_KEY: &[u8] = b"\x1b[15~";
pub const ENTER_KEY: &[u8] = b"\r";
pub const ESC_KEY: &[u8] = b"\x1b";
pub const DOWN_ARROW: &[u8] = b"\x1b[B";
pub const UP_ARROW: &[u8] = b"\x1b[A";
pub const RIGHT_ARROW: &[u8] = b"\x1b[C";
pub const LEFT_ARROW: &[u8] = b"\x1b[D";

/// Represents a menu item with its name and position
#[derive(Debug, Clone)]
pub struct MenuItem {
    pub name: String,
    pub position: usize,
}

/// Result type for telnet operations
pub type TelnetResult<T> = Result<T, String>;

/// Menu navigator that can dynamically discover and navigate menus
pub struct MenuNavigator {
    stream: TcpStream,
    current_menu_items: Vec<MenuItem>,
    current_position: usize,
}

impl MenuNavigator {
    /// Create a new menu navigator connected to the specified host
    pub fn new(host: &str) -> TelnetResult<Self> {
        Self::new_with_port(host, TELNET_PORT)
    }

    /// Create a new menu navigator with custom port
    pub fn new_with_port(host: &str, port: u16) -> TelnetResult<Self> {
        let addr = format!("{}:{}", host, port);
        log::info!("Telnet: Connecting to {}...", addr);

        let stream = TcpStream::connect_timeout(
            &addr
                .parse()
                .map_err(|e| format!("Invalid address: {}", e))?,
            Duration::from_secs(CONNECT_TIMEOUT_SECS),
        )
        .map_err(|e| format!("Telnet connect failed: {}", e))?;

        stream
            .set_read_timeout(Some(Duration::from_secs(IO_TIMEOUT_SECS)))
            .map_err(|e| format!("Set read timeout failed: {}", e))?;
        stream
            .set_write_timeout(Some(Duration::from_secs(IO_TIMEOUT_SECS)))
            .map_err(|e| format!("Set write timeout failed: {}", e))?;

        let mut nav = MenuNavigator {
            stream,
            current_menu_items: Vec::new(),
            current_position: 0,
        };

        // Read and discard initial screen
        let _ = nav.read_response();

        log::info!("Telnet: Connected successfully");
        Ok(nav)
    }

    /// Send a key sequence
    pub fn send_key(&mut self, key: &[u8]) -> TelnetResult<()> {
        self.stream
            .write_all(key)
            .map_err(|e| format!("Telnet write failed: {}", e))?;
        self.stream
            .flush()
            .map_err(|e| format!("Telnet flush failed: {}", e))?;
        std::thread::sleep(Duration::from_millis(200));
        Ok(())
    }

    /// Read response from telnet
    pub fn read_response(&mut self) -> TelnetResult<String> {
        let mut buffer = vec![0u8; 4096];
        let mut response = Vec::new();

        loop {
            match self.stream.read(&mut buffer) {
                Ok(0) => break,
                Ok(n) => {
                    response.extend_from_slice(&buffer[..n]);
                    std::thread::sleep(Duration::from_millis(100));
                }
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => break,
                Err(e) if e.kind() == std::io::ErrorKind::TimedOut => break,
                Err(e) => return Err(format!("Telnet read error: {}", e)),
            }
        }

        Ok(String::from_utf8_lossy(&response).to_string())
    }

    /// Strip ANSI codes from text
    pub fn strip_ansi(text: &str) -> String {
        // Simple ANSI stripping - handles most common escape sequences
        let mut result = String::new();
        let mut chars = text.chars().peekable();

        while let Some(c) = chars.next() {
            if c == '\x1b' {
                // Skip escape sequence
                if let Some(&next) = chars.peek() {
                    if next == '[' {
                        chars.next(); // consume '['
                        // Skip until we hit a letter
                        while let Some(&ch) = chars.peek() {
                            chars.next();
                            if ch.is_ascii_alphabetic() {
                                break;
                            }
                        }
                    } else if next == ']' {
                        chars.next(); // consume ']'
                        // Skip until BEL or ST
                        while let Some(&ch) = chars.peek() {
                            chars.next();
                            if ch == '\x07' {
                                break;
                            }
                        }
                    } else if next == 'O' || next == '(' || next == ')' {
                        chars.next(); // consume character
                        chars.next(); // consume next character
                    }
                }
            } else {
                result.push(c);
            }
        }

        result
    }

    /// Parse menu items from screen output
    pub fn parse_menu_items(&self, text: &str) -> Vec<MenuItem> {
        let cleaned = Self::strip_ansi(text);
        let mut items = Vec::new();

        // Known menu item names to look for
        let known_items = [
            "Power & Reset",
            "Built-in Drive A",
            "Built-in Drive B",
            "Software IEC",
            "Printer",
            "Configuration",
            "Streams",
            "Developer",
            "Return to Main Menu",
            "VIC Stream",   // Video output stream
            "Audio Stream", // Audio output stream
            "VLC Stream",   // Alternative name (older firmware)
        ];

        // Find known items in the text
        for known in known_items {
            if cleaned.contains(known) {
                if !items.iter().any(|item: &MenuItem| item.name == known) {
                    items.push(MenuItem {
                        name: known.to_string(),
                        position: items.len(),
                    });
                }
            }
        }

        // Sort by their position in the original text to maintain menu order
        if !items.is_empty() {
            items.sort_by_key(|item| cleaned.find(&item.name).unwrap_or(usize::MAX));
            // Reassign positions after sorting
            for (i, item) in items.iter_mut().enumerate() {
                item.position = i;
            }
            return items;
        }

        // Fallback: split by multiple spaces (fixed-width columns)
        let parts: Vec<&str> = cleaned
            .split("  ") // Two or more spaces
            .map(|s| s.trim())
            .filter(|s| !s.is_empty() && s.len() > 2)
            .collect();

        // Skip these status patterns
        let skip_patterns = [
            "Ready",
            "No media",
            "SD",
            "Flash",
            "Temp",
            "USB",
            "Send to",
            "F7=HELP",
            "F7-HELP",
            "---",
            "Internal Memory",
            "RAM Disk",
            "SanDisk",
            "Card",
            "lq",
            "mq",
            "xx",
            "kx",
        ];

        for part in parts {
            // Skip status lines
            if skip_patterns
                .iter()
                .any(|p| part.starts_with(p) || part.contains(p))
            {
                continue;
            }

            // Skip if too short or no alphabetic chars
            if part.len() < 3 || !part.chars().any(|c| c.is_alphabetic()) {
                continue;
            }

            // Skip box drawing characters
            if part
                .chars()
                .all(|c| "lqkmxj─│┌┐└┘├┤┬┴┼".contains(c) || !c.is_alphanumeric())
            {
                continue;
            }

            let name = part.to_string();
            if !items.iter().any(|item: &MenuItem| item.name == name) {
                items.push(MenuItem {
                    name,
                    position: items.len(),
                });
            }
        }

        items
    }

    /// Open the menu with F1 or F5
    /// Tries multiple key sequences to support different board types:
    /// - Ultimate64: F1 key
    /// - Elite II: F5 key
    pub fn open_menu(&mut self) -> TelnetResult<Vec<MenuItem>> {
        log::debug!("Telnet: Opening menu...");

        // Try different F1 escape sequences (for Ultimate64)
        let f1_sequences: &[(&[u8], &str)] = &[
            (F1_KEY_VT100, "F1 VT100"),
            (F1_KEY_XTERM, "F1 xterm"),
            (F1_KEY_LINUX, "F1 linux"),
        ];

        for (seq, name) in f1_sequences {
            log::trace!("Telnet: Trying {}...", name);
            self.send_key(seq)?;
            std::thread::sleep(Duration::from_millis(500));

            let response = self.read_response()?;
            let items = self.parse_menu_items(&response);

            if !items.is_empty() {
                log::debug!(
                    "Telnet: Menu opened with {}, found {} items",
                    name,
                    items.len()
                );
                self.current_menu_items = items.clone();
                self.current_position = 0;
                return Ok(items);
            }
        }

        // Try F5 as fallback (for Elite II)
        log::debug!("Telnet: F1 variants didn't work, trying F5 (Elite II)...");
        self.send_key(F5_KEY)?;
        std::thread::sleep(Duration::from_millis(500));

        let response = self.read_response()?;
        let items = self.parse_menu_items(&response);

        if !items.is_empty() {
            log::debug!("Telnet: Menu opened with F5, found {} items", items.len());
        } else {
            log::warn!("Telnet: Could not detect menu items");
        }

        self.current_menu_items = items.clone();
        self.current_position = 0;
        Ok(items)
    }

    /// Find menu item by name (case-insensitive, partial match)
    pub fn find_item(&self, name: &str) -> Option<&MenuItem> {
        let name_lower = name.to_lowercase();

        // Try exact match first
        if let Some(item) = self
            .current_menu_items
            .iter()
            .find(|item| item.name.to_lowercase() == name_lower)
        {
            return Some(item);
        }

        // Try partial match
        self.current_menu_items.iter().find(|item| {
            item.name.to_lowercase().contains(&name_lower)
                || name_lower.contains(&item.name.to_lowercase())
        })
    }

    /// Navigate to a menu item by name
    pub fn navigate_to(&mut self, name: &str) -> TelnetResult<()> {
        let target_position = self
            .find_item(name)
            .ok_or_else(|| {
                format!(
                    "Menu item '{}' not found. Available: {:?}",
                    name,
                    self.current_menu_items
                        .iter()
                        .map(|i| &i.name)
                        .collect::<Vec<_>>()
                )
            })?
            .position;

        log::debug!(
            "Telnet: Navigating to '{}' (position {})",
            name,
            target_position
        );

        // Calculate how many moves needed
        let moves = target_position as i32 - self.current_position as i32;

        if moves > 0 {
            for _ in 0..moves {
                self.send_key(DOWN_ARROW)?;
                std::thread::sleep(Duration::from_millis(100));
            }
        } else if moves < 0 {
            for _ in 0..moves.abs() {
                self.send_key(UP_ARROW)?;
                std::thread::sleep(Duration::from_millis(100));
            }
        }

        self.current_position = target_position;
        let _ = self.read_response(); // Clear buffer

        Ok(())
    }

    /// Open submenu (press right arrow) and parse its items
    pub fn open_submenu(&mut self) -> TelnetResult<Vec<MenuItem>> {
        log::debug!("Telnet: Opening submenu...");
        self.send_key(RIGHT_ARROW)?;
        std::thread::sleep(Duration::from_millis(400));

        let response = self.read_response()?;
        let items = self.parse_menu_items(&response);

        log::debug!("Telnet: Submenu has {} items", items.len());
        self.current_menu_items = items.clone();
        self.current_position = 0;

        Ok(items)
    }

    /// Press enter
    pub fn press_enter(&mut self) -> TelnetResult<String> {
        log::debug!("Telnet: Pressing ENTER...");
        self.send_key(ENTER_KEY)?;
        std::thread::sleep(Duration::from_millis(400));
        self.read_response()
    }

    /// Press enter twice (for confirming dialogs)
    pub fn press_enter_twice(&mut self) -> TelnetResult<String> {
        log::debug!("Telnet: Pressing ENTER twice...");
        self.send_key(ENTER_KEY)?;
        std::thread::sleep(Duration::from_millis(300));
        self.send_key(ENTER_KEY)?;
        std::thread::sleep(Duration::from_millis(400));
        self.read_response()
    }

    /// Press escape to close menu
    pub fn close_menu(&mut self) -> TelnetResult<()> {
        log::debug!("Telnet: Closing menu...");
        self.send_key(ESC_KEY)?;
        let _ = self.read_response();
        Ok(())
    }

    /// Go back (press left arrow)
    pub fn go_back(&mut self) -> TelnetResult<()> {
        log::debug!("Telnet: Going back...");
        self.send_key(LEFT_ARROW)?;
        std::thread::sleep(Duration::from_millis(200));
        let _ = self.read_response();
        Ok(())
    }

    /// Get current position in menu
    pub fn current_position(&self) -> usize {
        self.current_position
    }

    /// Get current menu items
    pub fn current_menu_items(&self) -> &[MenuItem] {
        &self.current_menu_items
    }

    /// Send a key and read the response
    pub fn send_key_and_read(&mut self, key: &[u8]) -> TelnetResult<String> {
        self.send_key(key)?;
        std::thread::sleep(Duration::from_millis(150));
        self.read_response()
    }

    /// Navigate to a position by index (relative movement from current position)
    pub fn navigate_to_position(&mut self, target: usize) -> TelnetResult<()> {
        let moves = target as i32 - self.current_position as i32;

        if moves > 0 {
            for _ in 0..moves {
                self.send_key(DOWN_ARROW)?;
                std::thread::sleep(Duration::from_millis(100));
            }
        } else if moves < 0 {
            for _ in 0..moves.abs() {
                self.send_key(UP_ARROW)?;
                std::thread::sleep(Duration::from_millis(100));
            }
        }

        self.current_position = target;
        let _ = self.read_response();
        Ok(())
    }

    /// Set the current position (for tracking without movement)
    pub fn set_position(&mut self, pos: usize) {
        self.current_position = pos;
    }

    /// Reset position to 0 (e.g., after directory change)
    pub fn reset_position(&mut self) {
        self.current_position = 0;
    }
}

/// Stream type for enabling/disabling
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StreamType {
    /// VIC video stream
    Video,
    /// Audio stream
    #[allow(dead_code)]
    Audio,
}

impl StreamType {
    /// Get the menu item name for this stream type
    fn menu_name(&self) -> &'static str {
        match self {
            StreamType::Video => "VIC Stream",
            StreamType::Audio => "Audio Stream",
        }
    }
}

/// Enable a stream on the Ultimate64 via telnet menu navigation
///
/// This function:
/// 1. Connects to the Ultimate64 via telnet
/// 2. Opens the menu (F1 for Ultimate64, F5 for Elite II)
/// 3. Navigates to Streams > [stream type]
/// 4. Presses Enter twice to enable (handles confirmation dialog)
/// 5. Closes the menu
pub fn enable_stream(host: &str, stream_type: StreamType) -> TelnetResult<()> {
    log::info!("Telnet: Enabling {:?} stream on {}...", stream_type, host);

    let mut nav = MenuNavigator::new(host)?;

    // Open menu
    let items = nav.open_menu()?;
    if items.is_empty() {
        return Err("Could not open menu - no items detected".to_string());
    }

    // Navigate to Streams
    nav.navigate_to("Streams")?;

    // Open Streams submenu
    let sub_items = nav.open_submenu()?;
    if sub_items.is_empty() {
        nav.close_menu()?;
        return Err("Could not open Streams submenu".to_string());
    }

    // Navigate to the specific stream
    nav.navigate_to(stream_type.menu_name())?;

    // Press Enter twice to enable (first enter opens, second confirms)
    nav.press_enter_twice()?;

    // Close menu
    nav.close_menu()?;

    log::info!("Telnet: {:?} stream enabled successfully", stream_type);
    Ok(())
}

/// Disable a stream on the Ultimate64 via telnet menu navigation
///
/// This function:
/// 1. Connects to the Ultimate64 via telnet
/// 2. Opens the menu (F1 for Ultimate64, F5 for Elite II)
/// 3. Navigates to Streams > [stream type]
/// 4. Presses Enter once to disable (toggle off)
/// 5. Closes the menu
pub fn disable_stream(host: &str, stream_type: StreamType) -> TelnetResult<()> {
    log::info!("Telnet: Disabling {:?} stream on {}...", stream_type, host);

    let mut nav = MenuNavigator::new(host)?;

    // Open menu
    let items = nav.open_menu()?;
    if items.is_empty() {
        return Err("Could not open menu - no items detected".to_string());
    }

    // Navigate to Streams
    nav.navigate_to("Streams")?;

    // Open Streams submenu
    let sub_items = nav.open_submenu()?;
    if sub_items.is_empty() {
        nav.close_menu()?;
        return Err("Could not open Streams submenu".to_string());
    }

    // Navigate to the specific stream
    nav.navigate_to(stream_type.menu_name())?;

    // Press Enter once to disable (single press toggles off)
    nav.press_enter()?;

    // Close menu
    nav.close_menu()?;

    log::info!("Telnet: {:?} stream disabled successfully", stream_type);
    Ok(())
}

/// Enable both video and audio streams
/// After enabling each stream, the menu closes, so we need to re-open for the second stream
pub fn enable_all_streams(host: &str) -> TelnetResult<()> {
    log::info!("Telnet: Enabling all streams on {}...", host);

    let mut nav = MenuNavigator::new(host)?;

    // === Enable VIC Stream ===
    // Open menu
    let items = nav.open_menu()?;
    if items.is_empty() {
        return Err("Could not open menu - no items detected".to_string());
    }

    // Navigate to Streams
    nav.navigate_to("Streams")?;

    // Open Streams submenu
    let sub_items = nav.open_submenu()?;
    if sub_items.is_empty() {
        nav.close_menu()?;
        return Err("Could not open Streams submenu".to_string());
    }

    // Navigate to VIC Stream (it's typically first, position 0)
    nav.navigate_to("VIC Stream")?;

    // Press Enter twice to enable
    nav.press_enter_twice()?;
    log::info!("Telnet: VIC Stream enabled");

    // Wait a moment for the menu to settle
    std::thread::sleep(Duration::from_millis(500));

    // === Enable Audio Stream ===
    // After enabling VIC Stream, menu has closed, so re-open everything
    let items = nav.open_menu()?;
    if items.is_empty() {
        return Err("Could not re-open menu for Audio Stream".to_string());
    }

    // Navigate to Streams again
    nav.navigate_to("Streams")?;

    // Open Streams submenu again
    let sub_items = nav.open_submenu()?;
    if sub_items.is_empty() {
        nav.close_menu()?;
        return Err("Could not re-open Streams submenu for Audio Stream".to_string());
    }

    // Navigate to Audio Stream
    nav.navigate_to("Audio Stream")?;

    // Press Enter twice to enable
    nav.press_enter_twice()?;
    log::info!("Telnet: Audio Stream enabled");

    // Close menu
    nav.close_menu()?;

    log::info!("Telnet: All streams enabled successfully");
    Ok(())
}

/// Disable both video and audio streams
/// After disabling each stream (single enter), the menu closes, so we need to re-open for the second stream
pub fn disable_all_streams(host: &str) -> TelnetResult<()> {
    log::info!("Telnet: Disabling all streams on {}...", host);

    let mut nav = MenuNavigator::new(host)?;

    // === Disable VIC Stream ===
    // Open menu
    let items = nav.open_menu()?;
    if items.is_empty() {
        return Err("Could not open menu - no items detected".to_string());
    }

    // Navigate to Streams
    nav.navigate_to("Streams")?;

    // Open Streams submenu
    let sub_items = nav.open_submenu()?;
    if sub_items.is_empty() {
        nav.close_menu()?;
        return Err("Could not open Streams submenu".to_string());
    }

    // Navigate to VIC Stream (it's typically first, position 0)
    nav.navigate_to("VIC Stream")?;

    // Press Enter once to disable
    nav.press_enter()?;
    log::info!("Telnet: VIC Stream disabled");

    // Wait a moment for the menu to settle
    std::thread::sleep(Duration::from_millis(500));

    // === Disable Audio Stream ===
    // After disabling VIC Stream, menu has closed, so re-open everything
    let items = nav.open_menu()?;
    if items.is_empty() {
        return Err("Could not re-open menu for Audio Stream".to_string());
    }

    // Navigate to Streams again
    nav.navigate_to("Streams")?;

    // Open Streams submenu again
    let sub_items = nav.open_submenu()?;
    if sub_items.is_empty() {
        nav.close_menu()?;
        return Err("Could not re-open Streams submenu for Audio Stream".to_string());
    }

    // Navigate to Audio Stream
    nav.navigate_to("Audio Stream")?;

    // Press Enter once to disable
    nav.press_enter()?;
    log::info!("Telnet: Audio Stream disabled");

    // Close menu
    nav.close_menu()?;

    log::info!("Telnet: All streams disabled successfully");
    Ok(())
}

/// Check if telnet connection to the host is possible
pub fn check_connection(host: &str) -> TelnetResult<bool> {
    match MenuNavigator::new(host) {
        Ok(_) => {
            log::debug!("Telnet: Connection to {} successful", host);
            Ok(true)
        }
        Err(e) => {
            log::debug!("Telnet: Connection to {} failed: {}", host, e);
            Ok(false)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_strip_ansi() {
        let input = "\x1b[32mHello\x1b[0m World";
        let output = MenuNavigator::strip_ansi(input);
        assert_eq!(output, "Hello World");
    }

    #[test]
    fn test_stream_type_menu_name() {
        assert_eq!(StreamType::Video.menu_name(), "VIC Stream");
        assert_eq!(StreamType::Audio.menu_name(), "Audio Stream");
    }
}
