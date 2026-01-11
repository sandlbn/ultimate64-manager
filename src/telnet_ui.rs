// Telnet UI - File Browser for Ultimate64
// Parses actual telnet output format:
// - Root menu: device listings (SD, Temp, USB1, etc.)
// - File listing: directories (name DIR) and files (name TYPE SIZE)
// Navigation: Right=Enter/Menu, Left=Back

use crate::telnet::{self, MenuNavigator, TelnetResult};
use iced::{
    Command, Element, Length,
    widget::{
        Column, Space, button, column, container, horizontal_rule, row, scrollable, text,
        text_input,
    },
};

/// Entry types in the browser
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EntryType {
    // Root menu entries (devices)
    Device { status: String },
    // File system entries
    Directory,
    File { file_type: String, size: String },
}

/// A parsed entry from telnet output
#[derive(Debug, Clone)]
pub struct BrowserEntry {
    pub name: String,
    pub entry_type: EntryType,
    pub index: usize,
}

/// Messages for the telnet UI
#[derive(Debug, Clone)]
pub enum TelnetUiMessage {
    // Connection
    HostChanged(String),
    Connect,
    Disconnect,

    // Navigation
    SelectEntry(usize), // Click to select (moves cursor)
    EnterEntry(usize),  // Double-click or Right arrow (enter dir / open menu)
    GoBack,             // Left arrow (go to parent)
    Refresh,

    // Menu mode (F1/F5)
    OpenF1Menu,
    OpenF5Menu,
    SelectMenuItem(usize), // Click on a menu item
    SendKey(NavKey),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NavKey {
    Up,
    Down,
    Left,
    Right,
    Enter,
    Escape,
}

impl NavKey {
    fn sequence(&self) -> &'static [u8] {
        match self {
            Self::Up => telnet::UP_ARROW,
            Self::Down => telnet::DOWN_ARROW,
            Self::Left => telnet::LEFT_ARROW,
            Self::Right => telnet::RIGHT_ARROW,
            Self::Enter => telnet::ENTER_KEY,
            Self::Escape => telnet::ESC_KEY,
        }
    }
}

/// Telnet browser state
pub struct TelnetUi {
    pub host: String,
    pub is_connected: bool,
    navigator: Option<MenuNavigator>,

    // Browser state
    pub entries: Vec<BrowserEntry>,
    pub selected_index: Option<usize>,
    pub current_path: String,
    pub status: String,

    // Menu mode (F1/F5 config menus)
    pub in_menu_mode: bool,
    pub menu_items: Vec<String>, // Parsed menu items
    pub menu_selected: usize,    // Currently selected menu item
}

impl Default for TelnetUi {
    fn default() -> Self {
        Self::new()
    }
}

impl TelnetUi {
    pub fn new() -> Self {
        Self {
            host: String::new(),
            is_connected: false,
            navigator: None,
            entries: Vec::new(),
            selected_index: None,
            current_path: String::new(),
            status: "Not connected".to_string(),
            in_menu_mode: false,
            menu_items: Vec::new(),
            menu_selected: 0,
        }
    }

    pub fn set_host(&mut self, host: String) {
        if !self.is_connected {
            self.host = host;
        }
    }

    /// Parse the telnet screen output into entries
    /// Handles both root menu (devices) and file listings
    fn parse_screen(content: &str) -> (Vec<BrowserEntry>, Option<String>) {
        let cleaned = MenuNavigator::strip_ansi(content);
        let mut entries = Vec::new();
        let mut current_path = None;

        // File type keywords
        let file_types = [
            "D64", "D71", "D81", "G64", "T64", "TAP", "PRG", "CRT", "SID", "REU", "BIN", "SEQ",
        ];

        for line in cleaned.lines() {
            let line = line.trim();

            // Skip empty lines
            if line.is_empty() {
                continue;
            }

            // Check for path at bottom (e.g., "/USB1/" or "/USB1/Games/")
            if line.starts_with('/') && !line.contains(' ') {
                current_path = Some(line.to_string());
                continue;
            }

            // Skip status/help lines
            if line.contains("F7")
                || line.contains("---")
                || line.starts_with("lq")
                || line.starts_with("mq")
            {
                continue;
            }

            // Parse the line by splitting on multiple spaces
            let parts: Vec<&str> = line.split_whitespace().collect();
            if parts.is_empty() {
                continue;
            }

            // Check if this is a "No media" line - skip it
            if line.contains("No media") {
                continue;
            }

            // Check for root menu format: "DeviceName  Description  Status"
            // e.g., "USB1    SanDisk 3.2Gen1    Ready"
            if parts.last() == Some(&"Ready") {
                // This is a device entry
                let name = parts[0].to_string();
                if !name.is_empty() && name != "Ready" {
                    entries.push(BrowserEntry {
                        name,
                        entry_type: EntryType::Device {
                            status: "Ready".to_string(),
                        },
                        index: entries.len(),
                    });
                }
                continue;
            }

            // Check for directory: "[name]" or "name DIR"
            if line.starts_with('[') && line.contains(']') {
                // Directory in bracket format: [CRT], [D64], etc.
                if let Some(end) = line.find(']') {
                    let name = line[1..end].to_string();
                    if !name.is_empty() {
                        entries.push(BrowserEntry {
                            name: format!("[{}]", name),
                            entry_type: EntryType::Directory,
                            index: entries.len(),
                        });
                    }
                }
                continue;
            }

            // Check for "name DIR" format (directory without brackets)
            if parts.len() >= 2 && parts.last() == Some(&"DIR") {
                let name = parts[..parts.len() - 1].join(" ");
                if !name.is_empty() {
                    entries.push(BrowserEntry {
                        name,
                        entry_type: EntryType::Directory,
                        index: entries.len(),
                    });
                }
                continue;
            }

            // Check for file: "name TYPE SIZE" format
            if parts.len() >= 3 {
                let last = parts[parts.len() - 1];
                let second_last = parts[parts.len() - 2];

                // Check if second_last is a known file type
                if file_types.contains(&second_last.to_uppercase().as_str()) {
                    let name = parts[..parts.len() - 2].join(" ");
                    if !name.is_empty() {
                        entries.push(BrowserEntry {
                            name,
                            entry_type: EntryType::File {
                                file_type: second_last.to_uppercase(),
                                size: last.to_string(),
                            },
                            index: entries.len(),
                        });
                    }
                    continue;
                }
            }

            // Check for file with 2 parts: "name TYPE" (no size shown)
            if parts.len() >= 2 {
                let last = parts[parts.len() - 1];
                if file_types.contains(&last.to_uppercase().as_str()) {
                    let name = parts[..parts.len() - 1].join(" ");
                    if !name.is_empty() {
                        entries.push(BrowserEntry {
                            name,
                            entry_type: EntryType::File {
                                file_type: last.to_uppercase(),
                                size: String::new(),
                            },
                            index: entries.len(),
                        });
                    }
                }
            }
        }

        // Re-index entries
        for (i, entry) in entries.iter_mut().enumerate() {
            entry.index = i;
        }

        (entries, current_path)
    }

    fn with_navigator<F, R>(&mut self, f: F) -> TelnetResult<R>
    where
        F: FnOnce(&mut MenuNavigator) -> TelnetResult<R>,
    {
        let nav = self
            .navigator
            .as_mut()
            .ok_or_else(|| "Not connected".to_string())?;
        f(nav)
    }

    pub fn update(&mut self, message: TelnetUiMessage) -> Command<TelnetUiMessage> {
        match message {
            TelnetUiMessage::HostChanged(host) => {
                self.host = host;
            }

            TelnetUiMessage::Connect => {
                if self.host.is_empty() {
                    self.status = "Enter host address".to_string();
                    return Command::none();
                }

                self.status = format!("Connecting to {}...", self.host);
                match MenuNavigator::new(&self.host) {
                    Ok(mut nav) => {
                        // Read initial screen (root menu)
                        if let Ok(content) = nav.read_response() {
                            let (entries, path) = Self::parse_screen(&content);
                            self.entries = entries;
                            if let Some(p) = path {
                                self.current_path = p;
                            }
                        }
                        self.navigator = Some(nav);
                        self.is_connected = true;
                        self.selected_index = if self.entries.is_empty() {
                            None
                        } else {
                            Some(0)
                        };
                        self.status = format!("Connected to {}", self.host);
                    }
                    Err(e) => {
                        self.status = format!("Connection failed: {}", e);
                    }
                }
            }

            TelnetUiMessage::Disconnect => {
                self.navigator = None;
                self.is_connected = false;
                self.entries.clear();
                self.selected_index = None;
                self.current_path.clear();
                self.in_menu_mode = false;
                self.menu_items.clear();
                self.menu_selected = 0;
                self.status = "Disconnected".to_string();
            }

            TelnetUiMessage::SelectEntry(index) => {
                // Navigate to this entry
                if let Some(current) = self.selected_index {
                    let moves = index as i32 - current as i32;
                    let _ = self.with_navigator(|nav| {
                        if moves > 0 {
                            for _ in 0..moves {
                                nav.send_key(telnet::DOWN_ARROW)?;
                                std::thread::sleep(std::time::Duration::from_millis(50));
                            }
                        } else if moves < 0 {
                            for _ in 0..moves.abs() {
                                nav.send_key(telnet::UP_ARROW)?;
                                std::thread::sleep(std::time::Duration::from_millis(50));
                            }
                        }
                        Ok(())
                    });
                }
                self.selected_index = Some(index);
            }

            TelnetUiMessage::EnterEntry(index) => {
                // First select the entry if not already selected
                if self.selected_index != Some(index) {
                    self.update(TelnetUiMessage::SelectEntry(index));
                }

                // Press Right to enter directory or open file menu
                match self.with_navigator(|nav| {
                    nav.send_key(telnet::RIGHT_ARROW)?;
                    std::thread::sleep(std::time::Duration::from_millis(200));
                    nav.read_response()
                }) {
                    Ok(content) => {
                        let (entries, path) = Self::parse_screen(&content);
                        self.entries = entries;
                        if let Some(p) = path {
                            self.current_path = p;
                        }
                        self.selected_index = if self.entries.is_empty() {
                            None
                        } else {
                            Some(0)
                        };
                        self.status = format!("{} items", self.entries.len());
                    }
                    Err(e) => self.status = format!("Error: {}", e),
                }
            }

            TelnetUiMessage::GoBack => {
                match self.with_navigator(|nav| {
                    nav.send_key(telnet::LEFT_ARROW)?;
                    std::thread::sleep(std::time::Duration::from_millis(200));
                    nav.read_response()
                }) {
                    Ok(content) => {
                        let (entries, path) = Self::parse_screen(&content);
                        self.entries = entries;
                        if let Some(p) = path {
                            self.current_path = p;
                        } else {
                            self.current_path.clear();
                        }
                        self.selected_index = if self.entries.is_empty() {
                            None
                        } else {
                            Some(0)
                        };
                        self.status = "Back".to_string();
                    }
                    Err(e) => self.status = format!("Error: {}", e),
                }
            }

            TelnetUiMessage::Refresh => {
                match self.with_navigator(|nav| nav.read_response()) {
                    Ok(content) => {
                        if self.in_menu_mode {
                            // Parse menu items
                            if let Some(nav) = &self.navigator {
                                let items = nav.parse_menu_items(&content);
                                if !items.is_empty() {
                                    self.menu_items =
                                        items.iter().map(|i| i.name.clone()).collect();
                                }
                            }
                        } else {
                            let (entries, path) = Self::parse_screen(&content);
                            self.entries = entries;
                            if let Some(p) = path {
                                self.current_path = p;
                            }
                        }
                        self.status = "Refreshed".to_string();
                    }
                    Err(e) => self.status = format!("Error: {}", e),
                }
            }

            TelnetUiMessage::OpenF1Menu => {
                match self.with_navigator(|nav| {
                    nav.send_key(telnet::F1_KEY_VT100)?;
                    std::thread::sleep(std::time::Duration::from_millis(300));
                    nav.read_response()
                }) {
                    Ok(content) => {
                        // Parse menu items using the navigator's parser
                        if let Some(nav) = &self.navigator {
                            let items = nav.parse_menu_items(&content);
                            self.menu_items = items.iter().map(|i| i.name.clone()).collect();
                        }
                        self.in_menu_mode = true;
                        self.menu_selected = 0;
                        self.status = format!("F1 Menu - {} items", self.menu_items.len());
                    }
                    Err(e) => self.status = format!("Error: {}", e),
                }
            }

            TelnetUiMessage::OpenF5Menu => {
                match self.with_navigator(|nav| {
                    nav.send_key(telnet::F5_KEY)?;
                    std::thread::sleep(std::time::Duration::from_millis(300));
                    nav.read_response()
                }) {
                    Ok(content) => {
                        // Parse menu items using the navigator's parser
                        if let Some(nav) = &self.navigator {
                            let items = nav.parse_menu_items(&content);
                            self.menu_items = items.iter().map(|i| i.name.clone()).collect();
                        }
                        self.in_menu_mode = true;
                        self.menu_selected = 0;
                        self.status = format!("F5 Menu - {} items", self.menu_items.len());
                    }
                    Err(e) => self.status = format!("Error: {}", e),
                }
            }

            TelnetUiMessage::SelectMenuItem(index) => {
                // Navigate to this menu item and enter it
                if index < self.menu_items.len() {
                    // Calculate moves from current position
                    let moves = index as i32 - self.menu_selected as i32;

                    match self.with_navigator(|nav| {
                        // Move to the item
                        if moves > 0 {
                            for _ in 0..moves {
                                nav.send_key(telnet::DOWN_ARROW)?;
                                std::thread::sleep(std::time::Duration::from_millis(50));
                            }
                        } else if moves < 0 {
                            for _ in 0..moves.abs() {
                                nav.send_key(telnet::UP_ARROW)?;
                                std::thread::sleep(std::time::Duration::from_millis(50));
                            }
                        }
                        // Enter the submenu
                        nav.send_key(telnet::RIGHT_ARROW)?;
                        std::thread::sleep(std::time::Duration::from_millis(200));
                        nav.read_response()
                    }) {
                        Ok(content) => {
                            // Parse new menu items
                            if let Some(nav) = &self.navigator {
                                let items = nav.parse_menu_items(&content);
                                self.menu_items = items.iter().map(|i| i.name.clone()).collect();
                            }
                            self.menu_selected = 0;
                            self.status = format!("{} items", self.menu_items.len());
                        }
                        Err(e) => self.status = format!("Error: {}", e),
                    }
                }
            }

            TelnetUiMessage::SendKey(key) => {
                match self.with_navigator(|nav| {
                    nav.send_key(key.sequence())?;
                    std::thread::sleep(std::time::Duration::from_millis(150));
                    nav.read_response()
                }) {
                    Ok(content) => {
                        if self.in_menu_mode {
                            if key == NavKey::Escape {
                                self.in_menu_mode = false;
                                self.menu_items.clear();
                                let (entries, path) = Self::parse_screen(&content);
                                self.entries = entries;
                                if let Some(p) = path {
                                    self.current_path = p;
                                }
                            } else {
                                // Update menu items from response
                                if let Some(nav) = &self.navigator {
                                    let items = nav.parse_menu_items(&content);
                                    if !items.is_empty() {
                                        self.menu_items =
                                            items.iter().map(|i| i.name.clone()).collect();
                                    }
                                }
                                // Update selection based on key
                                match key {
                                    NavKey::Up if self.menu_selected > 0 => {
                                        self.menu_selected -= 1;
                                    }
                                    NavKey::Down
                                        if self.menu_selected
                                            < self.menu_items.len().saturating_sub(1) =>
                                    {
                                        self.menu_selected += 1;
                                    }
                                    NavKey::Left => {
                                        self.menu_selected = 0;
                                    }
                                    _ => {}
                                }
                            }
                        } else {
                            let (entries, path) = Self::parse_screen(&content);
                            self.entries = entries;
                            if let Some(p) = path {
                                self.current_path = p;
                            }
                        }
                    }
                    Err(e) => self.status = format!("Error: {}", e),
                }
            }
        }
        Command::none()
    }

    pub fn view(&self) -> Element<'_, TelnetUiMessage> {
        if self.in_menu_mode {
            return self.view_menu_mode();
        }

        // Header
        let header = row![
            text("REMOTE")
                .size(14)
                .style(iced::theme::Text::Color(iced::Color::from_rgb(
                    0.5, 0.8, 1.0
                ))),
            Space::with_width(Length::Fill),
            text("[TELNET]")
                .size(11)
                .style(iced::theme::Text::Color(iced::Color::from_rgb(
                    0.5, 0.5, 0.5
                ))),
        ];

        // Connection row
        let connect_row = row![
            text_input("192.168.1.64", &self.host)
                .on_input(TelnetUiMessage::HostChanged)
                .width(Length::Fixed(140.0))
                .size(13),
            if self.is_connected {
                button(text("Disconnect").size(11))
                    .on_press(TelnetUiMessage::Disconnect)
                    .padding([4, 10])
                    .style(iced::theme::Button::Destructive)
            } else {
                button(text("Connect").size(11))
                    .on_press(TelnetUiMessage::Connect)
                    .padding([4, 10])
                    .style(iced::theme::Button::Primary)
            },
        ]
        .spacing(8)
        .align_items(iced::Alignment::Center);

        // Path display
        let path_text = if self.current_path.is_empty() {
            "/ (root)".to_string()
        } else {
            self.current_path.clone()
        };
        let path_row = container(text(&path_text).size(12).style(iced::theme::Text::Color(
            iced::Color::from_rgb(0.6, 0.8, 1.0),
        )))
        .padding([3, 8])
        .style(iced::theme::Container::Box);

        // Navigation buttons
        let nav_row = row![
            button(text("← Back").size(11))
                .on_press_maybe(self.is_connected.then_some(TelnetUiMessage::GoBack))
                .padding([4, 10])
                .style(iced::theme::Button::Primary),
            button(text("Refresh").size(11))
                .on_press_maybe(self.is_connected.then_some(TelnetUiMessage::Refresh))
                .padding([4, 10]),
            Space::with_width(Length::Fill),
            button(text("F1").size(11))
                .on_press_maybe(self.is_connected.then_some(TelnetUiMessage::OpenF1Menu))
                .padding([4, 8]),
            button(text("F5").size(11))
                .on_press_maybe(self.is_connected.then_some(TelnetUiMessage::OpenF5Menu))
                .padding([4, 8]),
        ]
        .spacing(6)
        .align_items(iced::Alignment::Center);

        // File/entry list
        let list = self.view_entry_list();

        // Status
        let status_row =
            text(&self.status)
                .size(10)
                .style(iced::theme::Text::Color(iced::Color::from_rgb(
                    0.5, 0.8, 0.5,
                )));

        column![
            header,
            horizontal_rule(1),
            connect_row,
            path_row,
            nav_row,
            horizontal_rule(1),
            list,
            horizontal_rule(1),
            status_row,
        ]
        .spacing(5)
        .padding(8)
        .height(Length::Fill)
        .into()
    }

    fn view_entry_list(&self) -> Element<'_, TelnetUiMessage> {
        if !self.is_connected {
            return container(text("Not connected").size(12))
                .width(Length::Fill)
                .height(Length::Fill)
                .center_x()
                .center_y()
                .into();
        }

        if self.entries.is_empty() {
            return container(text("Empty or loading...").size(12))
                .width(Length::Fill)
                .height(Length::Fill)
                .center_x()
                .center_y()
                .into();
        }

        let mut rows: Vec<Element<'_, TelnetUiMessage>> = Vec::new();

        for entry in &self.entries {
            let is_selected = self.selected_index == Some(entry.index);
            rows.push(self.view_entry_row(entry, is_selected));
        }

        scrollable(Column::with_children(rows).spacing(1).width(Length::Fill))
            .height(Length::Fill)
            .into()
    }

    fn view_entry_row(
        &self,
        entry: &BrowserEntry,
        is_selected: bool,
    ) -> Element<'_, TelnetUiMessage> {
        let bg_color = if is_selected {
            iced::Color::from_rgb(0.2, 0.35, 0.5)
        } else {
            iced::Color::TRANSPARENT
        };

        // Determine colors and type label based on entry type
        let (type_label, type_color, size_str) = match &entry.entry_type {
            EntryType::Device { status } => (
                status.as_str(),
                iced::Color::from_rgb(0.3, 0.9, 0.3),
                String::new(),
            ),
            EntryType::Directory => ("DIR", iced::Color::from_rgb(0.4, 0.7, 1.0), String::new()),
            EntryType::File { file_type, size } => {
                let color = match file_type.as_str() {
                    "D64" | "D71" | "D81" | "G64" => iced::Color::from_rgb(0.9, 0.8, 0.3),
                    "TAP" | "T64" => iced::Color::from_rgb(0.9, 0.5, 0.5),
                    "PRG" => iced::Color::from_rgb(0.5, 0.9, 0.5),
                    "CRT" => iced::Color::from_rgb(0.8, 0.5, 0.9),
                    "SID" => iced::Color::from_rgb(0.4, 0.9, 0.9),
                    "SEQ" => iced::Color::from_rgb(0.7, 0.7, 0.7),
                    _ => iced::Color::WHITE,
                };
                (file_type.as_str(), color, size.clone())
            }
        };

        // Entry name (clickable)
        let name_btn = button(
            text(&entry.name)
                .size(12)
                .style(iced::theme::Text::Color(type_color)),
        )
        .on_press(TelnetUiMessage::EnterEntry(entry.index))
        .padding([4, 8])
        .width(Length::Fill)
        .style(iced::theme::Button::Custom(Box::new(EntryRowStyle {
            bg_color,
        })));

        // Type label
        let type_text = text(type_label)
            .size(11)
            .width(Length::Fixed(50.0))
            .style(iced::theme::Text::Color(type_color));

        // Size (for files)
        let size_text =
            text(&size_str)
                .size(11)
                .width(Length::Fixed(50.0))
                .style(iced::theme::Text::Color(iced::Color::from_rgb(
                    0.6, 0.6, 0.6,
                )));

        // Action button (→ to enter/open menu)
        let action_btn = button(text("→").size(14))
            .on_press(TelnetUiMessage::EnterEntry(entry.index))
            .padding([2, 10])
            .style(iced::theme::Button::Secondary);

        row![name_btn, type_text, size_text, action_btn,]
            .spacing(4)
            .align_items(iced::Alignment::Center)
            .into()
    }

    fn view_menu_mode(&self) -> Element<'_, TelnetUiMessage> {
        let header = row![
            text("CONFIGURATION MENU").size(14),
            Space::with_width(Length::Fill),
            button(text("← Back").size(11))
                .on_press(TelnetUiMessage::SendKey(NavKey::Left))
                .padding([3, 8]),
            button(text("ESC").size(11))
                .on_press(TelnetUiMessage::SendKey(NavKey::Escape))
                .padding([3, 8])
                .style(iced::theme::Button::Secondary),
        ]
        .spacing(6);

        // Menu items as clickable list
        let menu_list: Element<'_, TelnetUiMessage> = if self.menu_items.is_empty() {
            container(text("No menu items found").size(12))
                .width(Length::Fill)
                .height(Length::Fill)
                .center_x()
                .center_y()
                .into()
        } else {
            let mut rows: Vec<Element<'_, TelnetUiMessage>> = Vec::new();

            for (index, item) in self.menu_items.iter().enumerate() {
                let is_selected = index == self.menu_selected;
                let bg_color = if is_selected {
                    iced::Color::from_rgb(0.2, 0.4, 0.6)
                } else {
                    iced::Color::TRANSPARENT
                };

                let item_btn =
                    button(
                        row![
                            text(format!("{}.", index + 1))
                                .size(11)
                                .width(Length::Fixed(25.0))
                                .style(iced::theme::Text::Color(iced::Color::from_rgb(
                                    0.5, 0.5, 0.5
                                ))),
                            text(item)
                                .size(12)
                                .style(iced::theme::Text::Color(if is_selected {
                                    iced::Color::from_rgb(0.9, 0.9, 0.3)
                                } else {
                                    iced::Color::WHITE
                                })),
                            Space::with_width(Length::Fill),
                            text("→").size(12).style(iced::theme::Text::Color(
                                iced::Color::from_rgb(0.5, 0.5, 0.5)
                            )),
                        ]
                        .spacing(8)
                        .align_items(iced::Alignment::Center),
                    )
                    .on_press(TelnetUiMessage::SelectMenuItem(index))
                    .padding([6, 12])
                    .width(Length::Fill)
                    .style(iced::theme::Button::Custom(Box::new(EntryRowStyle {
                        bg_color,
                    })));

                rows.push(item_btn.into());
            }

            scrollable(Column::with_children(rows).spacing(2).width(Length::Fill))
                .height(Length::Fill)
                .into()
        };

        let content = container(menu_list)
            .style(iced::theme::Container::Box)
            .padding(8)
            .width(Length::Fill)
            .height(Length::FillPortion(3));

        // D-pad navigation
        let nav = container(
            column![
                row![
                    Space::with_width(45),
                    button(text("▲").size(16))
                        .on_press(TelnetUiMessage::SendKey(NavKey::Up))
                        .padding([6, 14]),
                    Space::with_width(45),
                ],
                row![
                    button(text("◀").size(16))
                        .on_press(TelnetUiMessage::SendKey(NavKey::Left))
                        .padding([6, 14]),
                    button(text("OK").size(11))
                        .on_press(TelnetUiMessage::SendKey(NavKey::Enter))
                        .padding([6, 10])
                        .style(iced::theme::Button::Positive),
                    button(text("▶").size(16))
                        .on_press(TelnetUiMessage::SendKey(NavKey::Right))
                        .padding([6, 14]),
                ]
                .spacing(4),
                row![
                    Space::with_width(45),
                    button(text("▼").size(16))
                        .on_press(TelnetUiMessage::SendKey(NavKey::Down))
                        .padding([6, 14]),
                    Space::with_width(45),
                ],
            ]
            .spacing(4)
            .align_items(iced::Alignment::Center),
        )
        .width(Length::Fill)
        .center_x()
        .padding(10);

        // Status showing current selection
        let status = text(format!(
            "Selected: {} / {}",
            self.menu_selected + 1,
            self.menu_items.len()
        ))
        .size(10)
        .style(iced::theme::Text::Color(iced::Color::from_rgb(
            0.5, 0.8, 0.5,
        )));

        column![
            header,
            horizontal_rule(1),
            content,
            horizontal_rule(1),
            nav,
            status,
        ]
        .spacing(6)
        .padding(8)
        .height(Length::Fill)
        .into()
    }
}

/// Custom button style for entry rows
struct EntryRowStyle {
    bg_color: iced::Color,
}

impl iced::widget::button::StyleSheet for EntryRowStyle {
    type Style = iced::Theme;

    fn active(&self, _: &Self::Style) -> iced::widget::button::Appearance {
        iced::widget::button::Appearance {
            background: Some(iced::Background::Color(self.bg_color)),
            text_color: iced::Color::WHITE,
            border: iced::Border::default(),
            ..Default::default()
        }
    }

    fn hovered(&self, _: &Self::Style) -> iced::widget::button::Appearance {
        iced::widget::button::Appearance {
            background: Some(iced::Background::Color(iced::Color::from_rgb(
                0.25, 0.4, 0.55,
            ))),
            text_color: iced::Color::WHITE,
            border: iced::Border::default(),
            ..Default::default()
        }
    }

    fn pressed(&self, _: &Self::Style) -> iced::widget::button::Appearance {
        iced::widget::button::Appearance {
            background: Some(iced::Background::Color(iced::Color::from_rgb(
                0.3, 0.45, 0.6,
            ))),
            text_color: iced::Color::WHITE,
            border: iced::Border::default(),
            ..Default::default()
        }
    }
}
