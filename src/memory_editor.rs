use iced::{
    Task, Element, Length,
    widget::{
        Column, Row, Space, button, column, container,
        pick_list, row, scrollable, text, text_input, tooltip, rule,
    },
};
use std::sync::Arc;
use tokio::sync::Mutex as TokioMutex;
use ultimate64::Rest;

/// Timeout for REST API operations
const REST_TIMEOUT_SECS: u64 = 5;

/// Predefined C64 memory locations
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MemoryLocation {
    pub name: &'static str,
    pub address: u16,
    pub length: u16,
    pub description: &'static str,
}

impl std::fmt::Display for MemoryLocation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} (${:04X})", self.name, self.address)
    }
}

/// Common C64 memory locations
pub const MEMORY_LOCATIONS: &[MemoryLocation] = &[
    MemoryLocation {
        name: "Screen Memory",
        address: 0x0400,
        length: 0x400,
        description: "Default screen RAM",
    },
    MemoryLocation {
        name: "Color RAM",
        address: 0xD800,
        length: 0x400,
        description: "Screen color attributes",
    },
    MemoryLocation {
        name: "Sprite Pointers",
        address: 0x07F8,
        length: 0x08,
        description: "Sprite data pointers",
    },
    MemoryLocation {
        name: "VIC-II Registers",
        address: 0xD000,
        length: 0x40,
        description: "Video controller",
    },
    MemoryLocation {
        name: "SID Registers",
        address: 0xD400,
        length: 0x20,
        description: "Sound synthesizer",
    },
    MemoryLocation {
        name: "CIA #1",
        address: 0xDC00,
        length: 0x10,
        description: "Keyboard/joystick",
    },
    MemoryLocation {
        name: "CIA #2",
        address: 0xDD00,
        length: 0x10,
        description: "Serial/parallel",
    },
    MemoryLocation {
        name: "Kernal ROM",
        address: 0xE000,
        length: 0x2000,
        description: "System ROM",
    },
    MemoryLocation {
        name: "BASIC ROM",
        address: 0xA000,
        length: 0x2000,
        description: "BASIC interpreter",
    },
    MemoryLocation {
        name: "Character ROM",
        address: 0xD000,
        length: 0x1000,
        description: "Character patterns",
    },
    MemoryLocation {
        name: "Zero Page",
        address: 0x0000,
        length: 0x100,
        description: "Fast access memory",
    },
    MemoryLocation {
        name: "Stack",
        address: 0x0100,
        length: 0x100,
        description: "Processor stack",
    },
    MemoryLocation {
        name: "Keyboard Buffer",
        address: 0x0277,
        length: 0x0A,
        description: "Typed characters",
    },
    MemoryLocation {
        name: "BASIC Program",
        address: 0x0801,
        length: 0x200,
        description: "Default BASIC start",
    },
];

/// Display mode for memory view
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum DisplayMode {
    #[default]
    Hex,
    Ascii,
    Decimal,
    Binary,
}

impl std::fmt::Display for DisplayMode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DisplayMode::Hex => write!(f, "HEX"),
            DisplayMode::Ascii => write!(f, "ASCII"),
            DisplayMode::Decimal => write!(f, "DEC"),
            DisplayMode::Binary => write!(f, "BIN"),
        }
    }
}

/// Message type for memory editor events
#[derive(Debug, Clone)]
pub enum MemoryEditorMessage {
    // Address/length input
    AddressInputChanged(String),
    LengthInputChanged(String),

    // Memory operations
    ReadMemory,
    ReadMemoryComplete(Result<Vec<u8>, String>),
    WriteByteValueChanged(String),
    WriteByteConfirm,
    WriteByteCancel,
    WriteByteComplete(Result<(), String>),

    // Save/Load dumps
    SaveDump,
    SaveDumpPathSelected(Option<std::path::PathBuf>),
    SaveDumpComplete(Result<String, String>),
    LoadDump,
    LoadDumpPathSelected(Option<std::path::PathBuf>),
    LoadDumpComplete(Result<Vec<u8>, String>),
    WriteDumpToDevice,
    WriteDumpComplete(Result<(), String>),

    // Quick location selection
    LocationSelected(MemoryLocation),

    // Display mode
    DisplayModeChanged(DisplayMode),

    // Search
    SearchInputChanged(String),
    PerformSearch,
    ClearSearch,

    // Editing
    ByteClicked(usize), // offset in memory data

    // Clear/refresh
    ClearMemoryView,
    RefreshMemory,

    // Fill memory
    FillValueChanged(String),
    FillMemory,
    FillComplete(Result<(), String>),
}

/// Memory Editor state
pub struct MemoryEditor {
    // Current address and length
    current_address: u16,
    display_length: u16,

    // Input fields
    address_input: String,
    length_input: String,
    search_input: String,
    fill_value_input: String,

    // Memory data
    memory_data: Option<Vec<u8>>,

    // Pending load data (waiting for user confirmation to write)
    pending_load_data: Option<Vec<u8>>,

    // Display settings
    display_mode: DisplayMode,

    // Search results (offsets)
    search_matches: Vec<usize>,

    // Selected quick location
    selected_location: Option<MemoryLocation>,

    // Byte editing state
    editing_byte: Option<EditingByte>,

    // Loading state
    is_loading: bool,

    // Error/status message
    status_message: Option<String>,
}

/// State for editing a single byte
#[derive(Debug, Clone)]
struct EditingByte {
    offset: usize,
    original_value: u8,
    new_value_input: String,
}

impl MemoryEditor {
    pub fn new() -> Self {
        Self {
            current_address: 0x0400,
            display_length: 0x100,
            address_input: "0400".to_string(),
            length_input: "256".to_string(),
            search_input: String::new(),
            fill_value_input: "00".to_string(),
            memory_data: None,
            pending_load_data: None,
            display_mode: DisplayMode::Hex,
            search_matches: Vec::new(),
            selected_location: None,
            editing_byte: None,
            is_loading: false,
            status_message: None,
        }
    }

    pub fn update(
        &mut self,
        message: MemoryEditorMessage,
        connection: Option<Arc<TokioMutex<Rest>>>,
    ) -> Task<MemoryEditorMessage> {
        match message {
            MemoryEditorMessage::AddressInputChanged(value) => {
                // Only allow hex characters
                let filtered: String = value
                    .chars()
                    .filter(|c| c.is_ascii_hexdigit())
                    .take(4)
                    .collect();
                self.address_input = filtered.to_uppercase();

                if let Ok(addr) = u16::from_str_radix(&self.address_input, 16) {
                    self.current_address = addr;
                }
                Task::none()
            }

            MemoryEditorMessage::LengthInputChanged(value) => {
                // Only allow digits
                let filtered: String = value.chars().filter(|c| c.is_ascii_digit()).collect();
                self.length_input = filtered;

                if let Ok(len) = self.length_input.parse::<u16>() {
                    // Check that address + length doesn't exceed 0xFFFF
                    let max_len = 0xFFFFu16
                        .saturating_sub(self.current_address)
                        .saturating_add(1);
                    if len > 0 && len <= max_len {
                        self.display_length = len;
                    }
                }
                Task::none()
            }

            MemoryEditorMessage::LocationSelected(location) => {
                self.selected_location = Some(location.clone());
                self.current_address = location.address;
                self.display_length = location.length;
                self.address_input = format!("{:04X}", location.address);
                self.length_input = location.length.to_string();
                self.status_message = Some(format!("Selected: {}", location.description));

                // Auto-read the memory
                if connection.is_some() {
                    return self.update(MemoryEditorMessage::ReadMemory, connection);
                }
                Task::none()
            }

            MemoryEditorMessage::ReadMemory => {
                if let Some(conn) = connection {
                    self.is_loading = true;
                    self.status_message = Some("Reading memory...".to_string());
                    let address = self.current_address;
                    let length = self.display_length;

                    Task::perform(
                        async move { read_memory_async(conn, address, length).await },
                        MemoryEditorMessage::ReadMemoryComplete,
                    )
                } else {
                    self.status_message = Some("Not connected to Ultimate64".to_string());
                    Task::none()
                }
            }

            MemoryEditorMessage::ReadMemoryComplete(result) => {
                self.is_loading = false;
                match result {
                    Ok(data) => {
                        self.status_message = Some(format!(
                            "Read {} bytes from ${:04X}",
                            data.len(),
                            self.current_address
                        ));
                        self.memory_data = Some(data);
                        self.search_matches.clear();
                    }
                    Err(e) => {
                        self.status_message = Some(format!("Read failed: {}", e));
                    }
                }
                Task::none()
            }

            MemoryEditorMessage::ByteClicked(offset) => {
                if let Some(data) = &self.memory_data {
                    if offset < data.len() {
                        let value = data[offset];
                        self.editing_byte = Some(EditingByte {
                            offset,
                            original_value: value,
                            new_value_input: format!("{}", value),
                        });
                    }
                }
                Task::none()
            }

            MemoryEditorMessage::WriteByteValueChanged(value) => {
                if let Some(edit) = &mut self.editing_byte {
                    edit.new_value_input = value.chars().filter(|c| c.is_ascii_digit()).collect();
                }
                Task::none()
            }

            MemoryEditorMessage::WriteByteConfirm => {
                if let (Some(conn), Some(edit)) = (connection, &self.editing_byte) {
                    if let Ok(value) = edit.new_value_input.parse::<u8>() {
                        let address = self.current_address.wrapping_add(edit.offset as u16);
                        let offset = edit.offset;
                        let new_value = value;

                        self.is_loading = true;
                        self.status_message =
                            Some(format!("Writing ${:02X} to ${:04X}...", value, address));

                        return Task::perform(
                            async move {
                                write_byte_async(conn, address, new_value)
                                    .await
                                    .map(|_| (offset, new_value))
                            },
                            |result| match result {
                                Ok(_) => MemoryEditorMessage::WriteByteComplete(Ok(())),
                                Err(e) => MemoryEditorMessage::WriteByteComplete(Err(e)),
                            },
                        );
                    }
                }
                self.editing_byte = None;
                Task::none()
            }

            MemoryEditorMessage::WriteByteCancel => {
                self.editing_byte = None;
                Task::none()
            }

            MemoryEditorMessage::WriteByteComplete(result) => {
                self.is_loading = false;
                match result {
                    Ok(()) => {
                        // Update local data
                        if let (Some(data), Some(edit)) =
                            (&mut self.memory_data, &self.editing_byte)
                        {
                            if let Ok(value) = edit.new_value_input.parse::<u8>() {
                                if edit.offset < data.len() {
                                    data[edit.offset] = value;
                                    let address =
                                        self.current_address.wrapping_add(edit.offset as u16);
                                    self.status_message =
                                        Some(format!("Written ${:02X} to ${:04X}", value, address));
                                }
                            }
                        }
                    }
                    Err(e) => {
                        self.status_message = Some(format!("Write failed: {}", e));
                    }
                }
                self.editing_byte = None;
                Task::none()
            }

            MemoryEditorMessage::DisplayModeChanged(mode) => {
                self.display_mode = mode;
                Task::none()
            }

            MemoryEditorMessage::SearchInputChanged(value) => {
                self.search_input = value;
                Task::none()
            }

            MemoryEditorMessage::PerformSearch => {
                self.search_matches.clear();
                if let Some(data) = &self.memory_data {
                    if !self.search_input.is_empty() {
                        let matches = self.perform_search(data);
                        if matches.is_empty() {
                            self.status_message = Some("Pattern not found".to_string());
                        } else {
                            self.status_message =
                                Some(format!("Found {} match(es)", matches.len()));
                        }
                        self.search_matches = matches;
                    }
                }
                Task::none()
            }

            MemoryEditorMessage::ClearSearch => {
                self.search_input.clear();
                self.search_matches.clear();
                Task::none()
            }

            MemoryEditorMessage::ClearMemoryView => {
                self.memory_data = None;
                self.search_matches.clear();
                self.editing_byte = None;
                self.status_message = None;
                Task::none()
            }

            MemoryEditorMessage::RefreshMemory => {
                if self.memory_data.is_some() {
                    return self.update(MemoryEditorMessage::ReadMemory, connection);
                }
                Task::none()
            }

            MemoryEditorMessage::FillValueChanged(value) => {
                let filtered: String = value
                    .chars()
                    .filter(|c| c.is_ascii_hexdigit())
                    .take(2)
                    .collect();
                self.fill_value_input = filtered.to_uppercase();
                Task::none()
            }

            MemoryEditorMessage::FillMemory => {
                if let Some(conn) = connection {
                    if let Ok(value) = u8::from_str_radix(&self.fill_value_input, 16) {
                        self.is_loading = true;
                        let address = self.current_address;
                        let length = self.display_length;
                        self.status_message =
                            Some(format!("Filling {} bytes with ${:02X}...", length, value));

                        return Task::perform(
                            async move { fill_memory_async(conn, address, length, value).await },
                            MemoryEditorMessage::FillComplete,
                        );
                    }
                }
                Task::none()
            }

            MemoryEditorMessage::FillComplete(result) => {
                self.is_loading = false;
                match result {
                    Ok(()) => {
                        self.status_message = Some("Fill complete".to_string());
                        // Refresh to see changes
                        return self.update(MemoryEditorMessage::ReadMemory, connection);
                    }
                    Err(e) => {
                        self.status_message = Some(format!("Fill failed: {}", e));
                    }
                }
                Task::none()
            }

            // Save dump to file
            MemoryEditorMessage::SaveDump => {
                if self.memory_data.is_none() {
                    self.status_message =
                        Some("No memory data to save. Read memory first.".to_string());
                    return Task::none();
                }

                let default_name = format!(
                    "memdump_{:04X}_{:04X}.bin",
                    self.current_address,
                    self.current_address
                        .wrapping_add(self.display_length.saturating_sub(1))
                );

                Task::perform(
                    async move {
                        rfd::AsyncFileDialog::new()
                            .set_file_name(&default_name)
                            .add_filter("Binary dump", &["bin"])
                            .add_filter("All files", &["*"])
                            .save_file()
                            .await
                            .map(|handle| handle.path().to_path_buf())
                    },
                    MemoryEditorMessage::SaveDumpPathSelected,
                )
            }

            MemoryEditorMessage::SaveDumpPathSelected(path) => {
                if let Some(path) = path {
                    if let Some(data) = &self.memory_data {
                        let data = data.clone();

                        return Task::perform(
                            async move {
                                // Write binary data
                                std::fs::write(&path, &data)
                                    .map_err(|e| format!("Failed to save: {}", e))?;

                                Ok(format!(
                                    "Saved {} bytes to {}",
                                    data.len(),
                                    path.file_name().and_then(|n| n.to_str()).unwrap_or("file")
                                ))
                            },
                            MemoryEditorMessage::SaveDumpComplete,
                        );
                    }
                }
                Task::none()
            }

            MemoryEditorMessage::SaveDumpComplete(result) => {
                match result {
                    Ok(msg) => {
                        self.status_message = Some(msg);
                    }
                    Err(e) => {
                        self.status_message = Some(e);
                    }
                }
                Task::none()
            }

            // Load dump from file
            MemoryEditorMessage::LoadDump => Task::perform(
                async {
                    rfd::AsyncFileDialog::new()
                        .add_filter("Binary dump", &["bin"])
                        .add_filter("All files", &["*"])
                        .pick_file()
                        .await
                        .map(|handle| handle.path().to_path_buf())
                },
                MemoryEditorMessage::LoadDumpPathSelected,
            ),

            MemoryEditorMessage::LoadDumpPathSelected(path) => {
                if let Some(path) = path {
                    return Task::perform(
                        async move {
                            std::fs::read(&path).map_err(|e| format!("Failed to read file: {}", e))
                        },
                        MemoryEditorMessage::LoadDumpComplete,
                    );
                }
                Task::none()
            }

            MemoryEditorMessage::LoadDumpComplete(result) => {
                match result {
                    Ok(data) => {
                        let len = data.len();
                        self.pending_load_data = Some(data);
                        self.status_message = Some(format!(
                            "Loaded {} bytes. Click 'Write to Device' to write to ${:04X}",
                            len, self.current_address
                        ));
                    }
                    Err(e) => {
                        self.status_message = Some(e);
                    }
                }
                Task::none()
            }

            MemoryEditorMessage::WriteDumpToDevice => {
                if let (Some(conn), Some(data)) = (connection, self.pending_load_data.take()) {
                    self.is_loading = true;
                    let address = self.current_address;
                    let len = data.len();
                    self.status_message =
                        Some(format!("Writing {} bytes to ${:04X}...", len, address));

                    return Task::perform(
                        async move { write_memory_async(conn, address, data).await },
                        MemoryEditorMessage::WriteDumpComplete,
                    );
                } else {
                    self.status_message = Some("No data to write or not connected".to_string());
                }
                Task::none()
            }

            MemoryEditorMessage::WriteDumpComplete(result) => {
                self.is_loading = false;
                match result {
                    Ok(()) => {
                        self.status_message = Some("Memory written successfully!".to_string());
                        // Refresh to see changes
                        return self.update(MemoryEditorMessage::ReadMemory, connection);
                    }
                    Err(e) => {
                        self.status_message = Some(format!("Write failed: {}", e));
                    }
                }
                Task::none()
            }
        }
    }

    fn perform_search(&self, data: &[u8]) -> Vec<usize> {
        let search_text = self.search_input.trim().to_lowercase();
        let mut matches = Vec::new();

        // Try hex search first if it looks like hex
        let is_hex = search_text
            .chars()
            .all(|c| c.is_ascii_hexdigit() || c.is_whitespace());

        if is_hex && search_text.len() >= 2 {
            // Parse hex bytes
            let hex_clean: String = search_text.chars().filter(|c| !c.is_whitespace()).collect();
            if hex_clean.len() % 2 == 0 {
                let mut search_bytes = Vec::new();
                for chunk in hex_clean.as_bytes().chunks(2) {
                    if let Ok(byte_str) = std::str::from_utf8(chunk) {
                        if let Ok(byte) = u8::from_str_radix(byte_str, 16) {
                            search_bytes.push(byte);
                        }
                    }
                }

                if !search_bytes.is_empty() {
                    // Search for byte pattern
                    for i in 0..=data.len().saturating_sub(search_bytes.len()) {
                        if data[i..].starts_with(&search_bytes) {
                            matches.push(i);
                        }
                    }
                    return matches;
                }
            }
        }

        // Fall back to ASCII search
        let search_bytes = search_text.as_bytes();
        if !search_bytes.is_empty() {
            for i in 0..=data.len().saturating_sub(search_bytes.len()) {
                let window = &data[i..i + search_bytes.len()];
                // Case-insensitive ASCII search
                if window
                    .iter()
                    .zip(search_bytes)
                    .all(|(a, b)| a.to_ascii_lowercase() == *b)
                {
                    matches.push(i);
                }
            }
        }

        matches
    }

    pub fn view(&self, is_connected: bool, font_size: u32) -> Element<'_, MemoryEditorMessage> {
        let content: Element<'_, MemoryEditorMessage> = if !is_connected {
            column![
                Space::new().height(Length::Fill),
                text("Please connect to your Ultimate64 device first.").size(font_size),
                Space::new().height(Length::Fill),
            ]
            .align_x(iced::Alignment::Center)
            .width(Length::Fill)
            .into()
        } else {
            column![
                self.view_controls(font_size),
                rule::horizontal(1),
                if self.memory_data.is_some() {
                    self.view_memory_display(font_size)
                } else {
                    self.view_quick_locations(font_size)
                },
            ]
            .spacing(10)
            .into()
        };

        container(content)
            .width(Length::Fill)
            .height(Length::Fill)
            .padding(10)
            .into()
    }

    fn view_controls(&self, font_size: u32) -> Element<'_, MemoryEditorMessage> {
        let small_font = font_size.saturating_sub(2);

        // Address input
        let address_input = row![
            text("Address: $").size(small_font),
            text_input("0400", &self.address_input)
                .on_input(MemoryEditorMessage::AddressInputChanged)
                .width(Length::Fixed(60.0))
                .size(small_font),
        ]
        .spacing(5)
        .align_y(iced::Alignment::Center);

        // Length input
        let length_input = row![
            text("Length:").size(small_font),
            text_input("256", &self.length_input)
                .on_input(MemoryEditorMessage::LengthInputChanged)
                .width(Length::Fixed(60.0))
                .size(small_font),
            text("bytes").size(small_font),
        ]
        .spacing(5)
        .align_y(iced::Alignment::Center);

        // Read button
        let read_btn = button(text("Read").size(small_font))
            .on_press_maybe(if self.is_loading {
                None
            } else {
                Some(MemoryEditorMessage::ReadMemory)
            })
            .padding([5, 15]);

        // Quick location picker
        let location_picker = pick_list(
            MEMORY_LOCATIONS.to_vec(),
            self.selected_location.clone(),
            MemoryEditorMessage::LocationSelected,
        )
        .placeholder("Quick Locations...")
        .width(Length::Fixed(200.0))
        .text_size(small_font);

        // First row: address, length, read button, quick location
        let first_row = row![
            address_input,
            Space::new().width(Length::Fixed(20.0)),
            length_input,
            Space::new().width(Length::Fixed(10.0)),
            read_btn,
            Space::new().width(Length::Fixed(20.0)),
            location_picker,
        ]
        .spacing(10)
        .align_y(iced::Alignment::Center);

        // Search input
        let search_row = row![
            text("Search:").size(small_font),
            text_input("Hex or ASCII", &self.search_input)
                .on_input(MemoryEditorMessage::SearchInputChanged)
                .on_submit(MemoryEditorMessage::PerformSearch)
                .width(Length::Fixed(150.0))
                .size(small_font),
            button(text("Find").size(small_font))
                .on_press_maybe(
                    if self.search_input.is_empty() || self.memory_data.is_none() {
                        None
                    } else {
                        Some(MemoryEditorMessage::PerformSearch)
                    }
                )
                .padding([5, 10]),
            button(text("Clear").size(small_font))
                .on_press(MemoryEditorMessage::ClearSearch)
                .padding([5, 10]),
            Space::new().width(Length::Fixed(20.0)),
            text("Display:").size(small_font),
            pick_list(
                vec![
                    DisplayMode::Hex,
                    DisplayMode::Ascii,
                    DisplayMode::Decimal,
                    DisplayMode::Binary
                ],
                Some(self.display_mode),
                MemoryEditorMessage::DisplayModeChanged,
            )
            .text_size(small_font)
            .width(Length::Fixed(80.0)),
        ]
        .spacing(10)
        .align_y(iced::Alignment::Center);

        // Fill memory row
        let fill_row = row![
            text("Fill with: $").size(small_font),
            text_input("00", &self.fill_value_input)
                .on_input(MemoryEditorMessage::FillValueChanged)
                .width(Length::Fixed(40.0))
                .size(small_font),
            button(text("Fill Range").size(small_font))
                .on_press_maybe(if self.is_loading {
                    None
                } else {
                    Some(MemoryEditorMessage::FillMemory)
                })
                .padding([5, 10]),
            Space::new().width(Length::Fixed(20.0)),
            // Save/Load dump buttons
            button(text("Save Dump...").size(small_font))
                .on_press_maybe(if self.is_loading || self.memory_data.is_none() {
                    None
                } else {
                    Some(MemoryEditorMessage::SaveDump)
                })
                .padding([5, 10]),
            button(text("Load Dump...").size(small_font))
                .on_press_maybe(if self.is_loading {
                    None
                } else {
                    Some(MemoryEditorMessage::LoadDump)
                })
                .padding([5, 10]),
        ]
        .spacing(10)
        .align_y(iced::Alignment::Center);

        // Status row with optional "Write to Device" button
        let status_row: Element<'_, MemoryEditorMessage> = if self.pending_load_data.is_some() {
            row![
                text(format!(
                    "📁 {} bytes loaded, ready to write to ${:04X}",
                    self.pending_load_data
                        .as_ref()
                        .map(|d| d.len())
                        .unwrap_or(0),
                    self.current_address
                ))
                .size(small_font)
                .color(iced::Color::from_rgb(0.3, 0.8, 0.3)),
                Space::new().width(Length::Fixed(10.0)),
                button(text("Write to Device").size(small_font))
                    .on_press_maybe(if self.is_loading {
                        None
                    } else {
                        Some(MemoryEditorMessage::WriteDumpToDevice)
                    })
                    .padding([5, 15])
                    .style(button::primary),
                Space::new().width(Length::Fill),
            ]
            .spacing(10)
            .align_y(iced::Alignment::Center)
            .into()
        } else {
            row![
                if let Some(msg) = &self.status_message {
                    text(msg).size(small_font)
                } else {
                    text("").size(small_font)
                },
                Space::new().width(Length::Fill),
            ]
            .spacing(10)
            .align_y(iced::Alignment::Center)
            .into()
        };

        column![first_row, search_row, fill_row, status_row]
            .spacing(8)
            .into()
    }

    fn view_memory_display(&self, font_size: u32) -> Element<'_, MemoryEditorMessage> {
        let small_font = font_size.saturating_sub(2);
        let mono_font = font_size.saturating_sub(3);

        let Some(data) = &self.memory_data else {
            return Space::new().width(Length::Fill).height(Length::Fill).into();
        };

        // Header bar
        let header = row![
            text(format!(
                "Memory at ${:04X} - {} bytes",
                self.current_address,
                data.len()
            ))
            .size(small_font),
            Space::new().width(Length::Fill),
            button(text("Refresh").size(small_font))
                .on_press(MemoryEditorMessage::RefreshMemory)
                .padding([5, 10]),
            button(text("Close").size(small_font))
                .on_press(MemoryEditorMessage::ClearMemoryView)
                .padding([5, 10]),
        ]
        .spacing(10)
        .align_y(iced::Alignment::Center);

        // Build hex display rows
        let mut rows: Vec<Element<'_, MemoryEditorMessage>> = Vec::new();

        // Column headers
        let mut header_row =
            Row::new().push(text("ADDR").size(mono_font).width(Length::Fixed(50.0)));

        for i in 0..16 {
            header_row = header_row.push(
                text(format!("{:X}", i))
                    .size(mono_font)
                    .width(Length::Fixed(24.0)),
            );
        }
        header_row = header_row.push(Space::new().width(Length::Fixed(10.0)));
        header_row = header_row.push(text("ASCII").size(mono_font));
        rows.push(header_row.spacing(2).into());

        // Data rows
        for (row_idx, chunk) in data.chunks(16).enumerate() {
            let row_address = self.current_address.wrapping_add((row_idx * 16) as u16);
            let row_offset_base = row_idx * 16;

            let mut data_row = Row::new().push(
                text(format!("{:04X}", row_address))
                    .size(mono_font)
                    .width(Length::Fixed(50.0))
                    .color(iced::Color::from_rgb(0.4, 0.5, 0.9)),
            );

            // Hex bytes
            for (byte_idx, &byte) in chunk.iter().enumerate() {
                let offset = row_offset_base + byte_idx;
                let is_match = self.search_matches.contains(&offset);
                let is_editing = self
                    .editing_byte
                    .as_ref()
                    .map(|e| e.offset == offset)
                    .unwrap_or(false);

                let byte_text = match self.display_mode {
                    DisplayMode::Hex => format!("{:02X}", byte),
                    DisplayMode::Decimal => format!("{:3}", byte),
                    DisplayMode::Binary => format!("{:08b}", byte),
                    DisplayMode::Ascii => {
                        if byte >= 32 && byte <= 126 {
                            format!(" {} ", byte as char)
                        } else {
                            " . ".to_string()
                        }
                    }
                };

                let width = match self.display_mode {
                    DisplayMode::Hex => 24.0,
                    DisplayMode::Decimal => 30.0,
                    DisplayMode::Binary => 70.0,
                    DisplayMode::Ascii => 24.0,
                };

                let byte_widget = if is_editing {
                    container(text(byte_text.clone()).size(mono_font).color(iced::Color::BLACK))
                        .style(editing_style)
                        .width(Length::Fixed(width))
                } else if is_match {
                    container(text(byte_text.clone()).size(mono_font).color(iced::Color::BLACK))
                        .style(highlight_style)
                        .width(Length::Fixed(width))
                } else {
                    container(text(byte_text.clone()).size(mono_font)).width(Length::Fixed(width))
                };

                let tooltip_text = format!(
                    "${:04X}: {} ({})",
                    self.current_address.wrapping_add(offset as u16),
                    byte,
                    if byte >= 32 && byte <= 126 {
                        format!("'{}'", byte as char)
                    } else {
                        "non-printable".to_string()
                    }
                );

                data_row = data_row.push(tooltip(
                    button(byte_widget)
                        .on_press(MemoryEditorMessage::ByteClicked(offset))
                        .padding(0)
                        .style(button::text),
                    container(text(tooltip_text).size(small_font))
                        .padding(6)
                        .style(tooltip_style),
                    tooltip::Position::Bottom,
                ));
            }

            // Pad remaining columns if less than 16 bytes
            for _ in chunk.len()..16 {
                data_row = data_row.push(Space::new().width(Length::Fixed(24.0)));
            }

            // ASCII representation
            data_row = data_row.push(Space::new().width(Length::Fixed(10.0)));
            let ascii: String = chunk
                .iter()
                .map(|&b| if b >= 32 && b <= 126 { b as char } else { '.' })
                .collect();
            data_row = data_row.push(
                text(ascii)
                    .size(mono_font)
                    .color(iced::Color::from_rgb(0.6, 0.6, 0.6)),
            );

            rows.push(data_row.spacing(2).into());
        }

        // Byte editing dialog overlay
        let memory_content: Element<'_, MemoryEditorMessage> =
            scrollable(Column::with_children(rows).spacing(1))
                .height(Length::Fill)
                .into();

        let content = if let Some(edit) = &self.editing_byte {
            let address = self.current_address.wrapping_add(edit.offset as u16);
            let dialog = container(
                column![
                    text(format!("Edit byte at ${:04X}", address)).size(font_size),
                    rule::horizontal(1),
                    row![
                        text("Current:").size(small_font),
                        text(format!(
                            "${:02X} ({}) '{}'",
                            edit.original_value,
                            edit.original_value,
                            if edit.original_value >= 32 && edit.original_value <= 126 {
                                edit.original_value as char
                            } else {
                                '.'
                            }
                        ))
                        .size(small_font),
                    ]
                    .spacing(10),
                    row![
                        text("New value (0-255):").size(small_font),
                        text_input("0", &edit.new_value_input)
                            .on_input(MemoryEditorMessage::WriteByteValueChanged)
                            .on_submit(MemoryEditorMessage::WriteByteConfirm)
                            .width(Length::Fixed(80.0))
                            .size(small_font),
                    ]
                    .spacing(10),
                    row![
                        button(text("Write").size(small_font))
                            .on_press(MemoryEditorMessage::WriteByteConfirm)
                            .padding([5, 15]),
                        button(text("Cancel").size(small_font))
                            .on_press(MemoryEditorMessage::WriteByteCancel)
                            .padding([5, 15]),
                    ]
                    .spacing(10),
                ]
                .spacing(10)
                .padding(15),
            )
            .style(container::bordered_box)
            .width(Length::Fixed(300.0));

            column![
                header,
                rule::horizontal(1),
                container(column![
                    memory_content,
                    container(dialog)
                        .width(Length::Fill)
                        .center_x(Length::Fill)
                        .padding(20),
                ])
                .height(Length::Fill),
            ]
            .spacing(5)
            .into()
        } else {
            column![header, rule::horizontal(1), memory_content]
                .spacing(5)
                .into()
        };

        content
    }

    fn view_quick_locations(&self, font_size: u32) -> Element<'_, MemoryEditorMessage> {
        let small_font = font_size.saturating_sub(2);

        let title = text("Common C64 Memory Locations").size(font_size + 2);

        let subtitle = text("Click a location to view its contents")
            .size(small_font)
            .color(iced::Color::from_rgb(0.6, 0.6, 0.6));

        // Create location cards in a grid-like layout
        let mut rows: Vec<Element<'_, MemoryEditorMessage>> = Vec::new();

        for chunk in MEMORY_LOCATIONS.chunks(3) {
            let mut row_items = Row::new().spacing(10);

            for location in chunk {
                let card = button(
                    container(
                        column![
                            text(location.name).size(small_font),
                            text(location.description)
                                .size(small_font.saturating_sub(2))
                                .color(iced::Color::BLACK),
                            row![
                                text(format!("${:04X}", location.address))
                                    .size(small_font)
                                    .color(iced::Color::from_rgb(0.4, 0.5, 0.9)),
                                text(format!("{} bytes", location.length))
                                    .size(small_font.saturating_sub(2)),
                            ]
                            .spacing(10),
                        ]
                        .spacing(5)
                        .padding(15),
                    )
                    .width(Length::Fill),
                )
                .on_press(MemoryEditorMessage::LocationSelected(location.clone()))
                .style(button::secondary)
                .width(Length::Fill);

                row_items = row_items.push(card);
            }

            // Pad with empty space if less than 3 items in row
            for _ in chunk.len()..3 {
                row_items = row_items.push(Space::new().width(Length::Fill));
            }

            rows.push(row_items.width(Length::Fill).into());
        }

        scrollable(
            column![
                title,
                subtitle,
                Column::with_children(rows).spacing(10).width(Length::Fill)
            ]
            .spacing(15)
            .padding(10)
            .width(Length::Fill),
        )
        .height(Length::Fill)
        .width(Length::Fill)
        .into()
    }
}

// Custom container style functions for iced 0.14
fn highlight_style(_theme: &iced::Theme) -> container::Style {
    container::Style {
        background: Some(iced::Background::Color(iced::Color::from_rgb(
            1.0, 1.0, 0.0,
        ))),
        border: iced::Border::default(),
        text_color: Some(iced::Color::BLACK),
        shadow: iced::Shadow::default(),
        snap: false,
    }
}

fn editing_style(_theme: &iced::Theme) -> container::Style {
    container::Style {
        background: Some(iced::Background::Color(iced::Color::from_rgb(
            0.3, 0.8, 0.3,
        ))),
        border: iced::Border::default(),
        text_color: Some(iced::Color::BLACK),
        shadow: iced::Shadow::default(),
        snap: false,
    }
}

fn tooltip_style(_theme: &iced::Theme) -> container::Style {
    container::Style {
        background: Some(iced::Background::Color(iced::Color::from_rgb(
            0.2, 0.2, 0.25,
        ))),
        border: iced::Border {
            color: iced::Color::from_rgb(0.4, 0.4, 0.5),
            width: 1.0,
            radius: 4.0.into(),
        },
        text_color: Some(iced::Color::WHITE),
        shadow: iced::Shadow::default(),
        snap: false,
    }
}

// Async helper functions for memory operations
async fn read_memory_async(
    connection: Arc<TokioMutex<Rest>>,
    address: u16,
    length: u16,
) -> Result<Vec<u8>, String> {
    let result = tokio::time::timeout(
        tokio::time::Duration::from_secs(REST_TIMEOUT_SECS),
        tokio::task::spawn_blocking(move || {
            let conn = connection.blocking_lock();
            conn.read_mem(address, length)
                .map_err(|e| format!("Read failed: {}", e))
        }),
    )
    .await;

    match result {
        Ok(Ok(data)) => data,
        Ok(Err(e)) => Err(format!("Task error: {}", e)),
        Err(_) => Err("Read timed out - device may be offline".to_string()),
    }
}

async fn write_byte_async(
    connection: Arc<TokioMutex<Rest>>,
    address: u16,
    value: u8,
) -> Result<(), String> {
    let result = tokio::time::timeout(
        tokio::time::Duration::from_secs(REST_TIMEOUT_SECS),
        tokio::task::spawn_blocking(move || {
            let conn = connection.blocking_lock();
            conn.write_mem(address, &[value])
                .map_err(|e| format!("Write failed: {}", e))
        }),
    )
    .await;

    match result {
        Ok(Ok(r)) => r,
        Ok(Err(e)) => Err(format!("Task error: {}", e)),
        Err(_) => Err("Write timed out - device may be offline".to_string()),
    }
}

async fn fill_memory_async(
    connection: Arc<TokioMutex<Rest>>,
    address: u16,
    length: u16,
    value: u8,
) -> Result<(), String> {
    let result = tokio::time::timeout(
        tokio::time::Duration::from_secs(REST_TIMEOUT_SECS * 2), // Longer timeout for fill
        tokio::task::spawn_blocking(move || {
            let conn = connection.blocking_lock();
            // Fill by writing chunks
            let chunk_size = 256usize;
            let fill_data: Vec<u8> = vec![value; chunk_size];

            let mut offset = 0u16;
            while offset < length {
                let remaining = (length - offset) as usize;
                let write_size = remaining.min(chunk_size);
                let current_addr = address.wrapping_add(offset);

                conn.write_mem(current_addr, &fill_data[..write_size])
                    .map_err(|e| format!("Fill failed at ${:04X}: {}", current_addr, e))?;

                offset += write_size as u16;
            }
            Ok(())
        }),
    )
    .await;

    match result {
        Ok(Ok(r)) => r,
        Ok(Err(e)) => Err(format!("Task error: {}", e)),
        Err(_) => Err("Fill timed out - device may be offline".to_string()),
    }
}

async fn write_memory_async(
    connection: Arc<TokioMutex<Rest>>,
    address: u16,
    data: Vec<u8>,
) -> Result<(), String> {
    let result = tokio::time::timeout(
        tokio::time::Duration::from_secs(REST_TIMEOUT_SECS * 4), // Longer timeout for large writes
        tokio::task::spawn_blocking(move || {
            let conn = connection.blocking_lock();
            // Write in chunks (API max is typically 256 bytes per call)
            let chunk_size = 256usize;

            let mut offset = 0usize;
            while offset < data.len() {
                let remaining = data.len() - offset;
                let write_size = remaining.min(chunk_size);
                let current_addr = address.wrapping_add(offset as u16);

                conn.write_mem(current_addr, &data[offset..offset + write_size])
                    .map_err(|e| format!("Write failed at ${:04X}: {}", current_addr, e))?;

                offset += write_size;
            }
            Ok(())
        }),
    )
    .await;

    match result {
        Ok(Ok(r)) => r,
        Ok(Err(e)) => Err(format!("Task error: {}", e)),
        Err(_) => Err("Write timed out - device may be offline".to_string()),
    }
}