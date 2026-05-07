use iced::{
    widget::{
        button, column, container, pick_list, row, rule, scrollable, text, text_input, tooltip,
        Column, Row, Space,
    },
    Element, Length, Subscription, Task,
};
use std::sync::Arc;
use tokio::sync::Mutex as TokioMutex;
use ultimate64::Rest;

use crate::port64;

// ─────────────────────────────────────────────────────────────────
//  Memory locations
// ─────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Eq)]
/// Length is u32 throughout the editor: the C64 has 64 KB = 65536 bytes
/// total, which doesn't fit in u16 (max 65535). Storing as u32 lets the user
/// dump the entire address space in one request, matching what the REST API
/// already accepts (`?length=65536` works on the device).
pub struct MemoryLocation {
    pub name: &'static str,
    pub address: u16,
    pub length: u32,
    pub description: &'static str,
}

impl std::fmt::Display for MemoryLocation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} (${:04X})", self.name, self.address)
    }
}

pub const MEMORY_LOCATIONS: &[MemoryLocation] = &[
    // ── CPU / Zero page ───────────────────────────────────────────
    MemoryLocation {
        name: "Zero Page",
        address: 0x0000,
        length: 0x100,
        description: "Fast-access CPU variables",
    },
    MemoryLocation {
        name: "Stack",
        address: 0x0100,
        length: 0x100,
        description: "6510 hardware stack",
    },
    MemoryLocation {
        name: "Keyboard Buffer",
        address: 0x0277,
        length: 0x0A,
        description: "Typed characters ($C6 = count)",
    },
    MemoryLocation {
        name: "I/O Vectors",
        address: 0x0300,
        length: 0x10,
        description: "Soft KERNAL vectors (BASIN at $0302)",
    },
    MemoryLocation {
        name: "IRQ Vector",
        address: 0x0314,
        length: 0x10,
        description: "$0314/15=IRQ $0316/17=BRK $0318/19=NMI",
    },
    // ── BASIC / program area ──────────────────────────────────────
    MemoryLocation {
        name: "BASIC Program",
        address: 0x0801,
        length: 0x0200,
        description: "Default BASIC program start",
    },
    MemoryLocation {
        name: "BASIC Pointers",
        address: 0x002B,
        length: 0x0A,
        description: "TXTTAB/VARTAB/ARYTAB/STREND/FRETOP",
    },
    // ── Screen / colour / sprites ─────────────────────────────────
    MemoryLocation {
        name: "Screen Memory",
        address: 0x0400,
        length: 0x0400,
        description: "Default screen RAM (40x25 chars)",
    },
    MemoryLocation {
        name: "Color RAM",
        address: 0xD800,
        length: 0x0400,
        description: "Screen colour nibbles (nybble per char)",
    },
    MemoryLocation {
        name: "Sprite Pointers",
        address: 0x07F8,
        length: 0x08,
        description: "8 sprite data pointers (x64 = address)",
    },
    MemoryLocation {
        name: "Sprite Data",
        address: 0x0340,
        length: 0x0200,
        description: "Default sprite data area (8 x 64 bytes)",
    },
    // ── VIC-II ────────────────────────────────────────────────────
    MemoryLocation {
        name: "VIC-II Registers",
        address: 0xD000,
        length: 0x40,
        description: "Full VIC-II register set",
    },
    MemoryLocation {
        name: "VIC Colours",
        address: 0xD020,
        length: 0x0F,
        description: "Border + background colour regs",
    },
    MemoryLocation {
        name: "VIC Sprites",
        address: 0xD000,
        length: 0x20,
        description: "Sprite position/enable regs",
    },
    // ── SID (all common locations) ────────────────────────────────
    MemoryLocation {
        name: "SID #1 $D400",
        address: 0xD400,
        length: 0x20,
        description: "Primary SID chip (always present)",
    },
    MemoryLocation {
        name: "SID #2 $D500",
        address: 0xD500,
        length: 0x20,
        description: "2nd SID - Prophet64 / HardSID",
    },
    MemoryLocation {
        name: "SID #2 $D420",
        address: 0xD420,
        length: 0x20,
        description: "2nd SID - SidCard / SIDCARD2",
    },
    MemoryLocation {
        name: "SID #2 $DE00",
        address: 0xDE00,
        length: 0x20,
        description: "2nd SID - I/O expansion area 1",
    },
    MemoryLocation {
        name: "SID #2 $DF00",
        address: 0xDF00,
        length: 0x20,
        description: "2nd SID - I/O expansion area 2",
    },
    MemoryLocation {
        name: "SID #3 $D440",
        address: 0xD440,
        length: 0x20,
        description: "3rd SID - triple-SID configs",
    },
    // ── CIA chips ─────────────────────────────────────────────────
    MemoryLocation {
        name: "CIA #1",
        address: 0xDC00,
        length: 0x10,
        description: "Keyboard/joystick/IRQ timers",
    },
    MemoryLocation {
        name: "CIA #2",
        address: 0xDD00,
        length: 0x10,
        description: "Serial port/NMI/VIC bank select",
    },
    MemoryLocation {
        name: "CIA1 Timer A",
        address: 0xDC04,
        length: 0x04,
        description: "Timer A lo/hi + Timer B lo/hi",
    },
    // ── ROM areas ─────────────────────────────────────────────────
    MemoryLocation {
        name: "Kernal ROM",
        address: 0xE000,
        length: 0x2000,
        description: "8 KB system ROM (replaceable via Ultimate)",
    },
    MemoryLocation {
        name: "BASIC ROM",
        address: 0xA000,
        length: 0x2000,
        description: "8 KB BASIC interpreter ROM",
    },
    MemoryLocation {
        name: "Character ROM",
        address: 0xD000,
        length: 0x1000,
        description: "4 KB built-in character set",
    },
    // ── Hardware vectors ──────────────────────────────────────────
    MemoryLocation {
        name: "NMI Vector",
        address: 0xFFFA,
        length: 0x02,
        description: "Hardware NMI vector (ROM)",
    },
    MemoryLocation {
        name: "RESET Vector",
        address: 0xFFFC,
        length: 0x02,
        description: "Hardware RESET vector (ROM)",
    },
    MemoryLocation {
        name: "IRQ/BRK Vector",
        address: 0xFFFE,
        length: 0x02,
        description: "Hardware IRQ/BRK vector (ROM)",
    },
];

// ─────────────────────────────────────────────────────────────────
//  Address space selector
// ─────────────────────────────────────────────────────────────────

/// Which memory bus to target
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum AddressSpace {
    /// Normal 64 KB C64 RAM (REST read_mem / DMAWRITE write)
    #[default]
    C64Ram,
    /// REU expansion RAM up to 16 MB (SOCKET_CMD_REUWRITE, raw socket)
    Reu,
}

impl std::fmt::Display for AddressSpace {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AddressSpace::C64Ram => write!(f, "C64 RAM"),
            AddressSpace::Reu => write!(f, "REU"),
        }
    }
}

// ─────────────────────────────────────────────────────────────────
//  Display mode
// ─────────────────────────────────────────────────────────────────

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

// ─────────────────────────────────────────────────────────────────
//  Bookmark
// ─────────────────────────────────────────────────────────────────

/// A user-defined named memory range
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct Bookmark {
    pub label: String,
    pub address: u32, // u32 to cover REU 24-bit space too
    pub length: u32,
    pub space: BookmarkSpace,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum BookmarkSpace {
    C64Ram,
    Reu,
}

impl std::fmt::Display for Bookmark {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let prefix = match self.space {
            BookmarkSpace::C64Ram => "",
            BookmarkSpace::Reu => "REU:",
        };
        write!(f, "{} ({}${:04X})", self.label, prefix, self.address)
    }
}

// ─────────────────────────────────────────────────────────────────
//  Undo / redo entry
// ─────────────────────────────────────────────────────────────────

/// One reversible byte-write in C64 RAM space
#[derive(Debug, Clone)]
struct UndoEntry {
    address: u16,
    old_value: u8,
    new_value: u8,
    /// Offset inside `memory_data` so we can patch the local copy instantly
    offset: usize,
}

// ─────────────────────────────────────────────────────────────────
//  Flash info returned by READFLASH
// ─────────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct FlashInfo {
    pub page_size: u32,
    pub page_count: u32,
    pub pages: Vec<Vec<u8>>, // pages fetched so far
    pub current_page: u32,
}

// ─────────────────────────────────────────────────────────────────
//  Messages
// ─────────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub enum MemoryEditorMessage {
    // Address / length / space
    AddressInputChanged(String),
    LengthInputChanged(String),
    AddressSpaceChanged(AddressSpace),

    // Read/write
    ReadMemory,
    ReadMemoryComplete(Result<Vec<u8>, String>),
    WriteByteValueChanged(String),
    WriteByteConfirm,
    WriteByteCancel,
    WriteByteComplete(Result<(usize, u8), String>),

    // Undo / Redo
    Undo,
    Redo,

    // Fill memory
    FillValueChanged(String),
    FillMemory,
    FillComplete(Result<(), String>),

    // Save / Load dumps
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

    // Byte click / edit
    ByteClicked(usize),

    // Clear / refresh
    ClearMemoryView,
    RefreshMemory,

    // Watch mode
    ToggleWatch,
    WatchTick,

    // Bookmarks
    BookmarkLabelChanged(String),
    AddBookmark,
    BookmarkSelected(Bookmark),
    DeleteBookmark(usize),

    // ── Raw socket DMA commands ──────────────────────────────────
    /// SOCKET_CMD_DMAWRITE — write `data` to C64 address `offset` via raw TCP port-64
    DmaWrite {
        host: String,
        password: Option<String>,
        offset: u16,
        data: Vec<u8>,
    },
    DmaWriteComplete(Result<(), String>),

    /// SOCKET_CMD_DMAJUMP — like DmaWrite but triggers execution at the target address
    DmaJump {
        host: String,
        password: Option<String>,
        offset: u16,
        data: Vec<u8>,
    },
    DmaJumpComplete(Result<(), String>),

    /// SOCKET_CMD_REUWRITE — write into REU address space
    ReuWrite {
        host: String,
        password: Option<String>,
        reu_offset: u32, // 24-bit REU address
        data: Vec<u8>,
    },
    ReuWriteComplete(Result<(), String>),

    /// SOCKET_CMD_KERNALWRITE — replace the active Kernal ROM image
    KernalWriteClicked,
    KernalWritePathSelected(Option<std::path::PathBuf>),
    KernalWriteComplete(Result<(), String>),

    /// SOCKET_CMD_READFLASH — inspect flash memory pages
    ReadFlashInfo,
    FlashInfoComplete(Result<(u32, u32), String>), // (page_size, page_count)
    ReadFlashPage(u32),
    FlashPageComplete(Result<(u32, Vec<u8>), String>),
    FlashPageChanged(String),
}

// ─────────────────────────────────────────────────────────────────
//  State for byte editing
// ─────────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
struct EditingByte {
    offset: usize,
    original_value: u8,
    new_value_input: String,
}

// ─────────────────────────────────────────────────────────────────
//  Main editor state
// ─────────────────────────────────────────────────────────────────

pub struct MemoryEditor {
    // Address / length / space
    current_address: u32, // u32 covers both C64 (16-bit) and REU (24-bit)
    display_length: u32,
    address_space: AddressSpace,

    // Input fields
    address_input: String,
    length_input: String,
    search_input: String,
    fill_value_input: String,

    // Memory data
    memory_data: Option<Vec<u8>>,
    pending_load_data: Option<Vec<u8>>,

    // Display
    display_mode: DisplayMode,
    search_matches: Vec<usize>,
    selected_location: Option<MemoryLocation>,
    editing_byte: Option<EditingByte>,

    // Undo / Redo stacks (only for C64 RAM byte writes)
    undo_stack: Vec<UndoEntry>,
    redo_stack: Vec<UndoEntry>,

    // Watch / live-refresh
    watch_active: bool,

    // Bookmarks
    bookmarks: Vec<Bookmark>,
    bookmark_label_input: String,
    selected_bookmark: Option<usize>,

    // Flash inspector
    flash_info: Option<FlashInfo>,
    flash_page_input: String,

    // Kernal write
    kernal_pending_path: Option<std::path::PathBuf>,

    // Loading / busy
    is_loading: bool,
    status_message: Option<String>,
}

impl MemoryEditor {
    pub fn new() -> Self {
        Self {
            current_address: 0x0400,
            display_length: 0x100,
            address_space: AddressSpace::C64Ram,
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
            undo_stack: Vec::new(),
            redo_stack: Vec::new(),
            watch_active: false,
            bookmarks: Vec::new(),
            bookmark_label_input: String::new(),
            selected_bookmark: None,
            flash_info: None,
            flash_page_input: "0".to_string(),
            kernal_pending_path: None,
            is_loading: false,
            status_message: None,
        }
    }

    // ── Subscription: watch ticker ───────────────────────────────

    /// Merged into the app's `Subscription::batch`. When watch is on, fires
    /// `WatchTick` every 500 ms so the UI refreshes live.
    pub fn subscription(&self) -> Subscription<MemoryEditorMessage> {
        if self.watch_active && self.memory_data.is_some() {
            iced::time::every(std::time::Duration::from_millis(500))
                .map(|_| MemoryEditorMessage::WatchTick)
        } else {
            Subscription::none()
        }
    }

    // ── Update ───────────────────────────────────────────────────

    /// `host` and `password` are needed by the raw-socket helpers.
    /// Pass `self.settings.connection.host.clone()` and
    /// `self.settings.connection.password.clone()` from the parent update.
    pub fn update(
        &mut self,
        message: MemoryEditorMessage,
        connection: Option<Arc<TokioMutex<Rest>>>,
        host: Option<String>,
        password: Option<String>,
    ) -> Task<MemoryEditorMessage> {
        match message {
            // ── Address / length / space ─────────────────────────
            MemoryEditorMessage::AddressSpaceChanged(space) => {
                self.address_space = space;
                // Reset address input width to 6 hex chars for REU (24-bit)
                let max_digits = if space == AddressSpace::Reu { 6 } else { 4 };
                let filtered: String = self
                    .address_input
                    .chars()
                    .filter(|c| c.is_ascii_hexdigit())
                    .take(max_digits)
                    .collect();
                self.address_input = filtered.to_uppercase();
                if let Ok(addr) = u32::from_str_radix(&self.address_input, 16) {
                    self.current_address = addr;
                }
                Task::none()
            }

            MemoryEditorMessage::AddressInputChanged(value) => {
                let max_digits = if self.address_space == AddressSpace::Reu {
                    6
                } else {
                    4
                };
                let filtered: String = value
                    .chars()
                    .filter(|c| c.is_ascii_hexdigit())
                    .take(max_digits)
                    .collect();
                self.address_input = filtered.to_uppercase();
                if let Ok(addr) = u32::from_str_radix(&self.address_input, 16) {
                    self.current_address = addr;
                }
                Task::none()
            }

            MemoryEditorMessage::LengthInputChanged(value) => {
                // Accept decimal *and* hex: `1024`, `$400`, `0x400`, `400h`.
                // Hex letters in plain digits also flip to hex
                // (`100` is decimal 100; `1ff` is hex 0x1FF) — same
                // behavior as the address field. Cap at 65536 (= one full
                // C64 address space) since the REST API rejects bigger.
                let filtered: String = value
                    .chars()
                    .filter(|c| {
                        c.is_ascii_hexdigit()
                            || matches!(c, '$' | 'x' | 'X' | 'h' | 'H')
                    })
                    .collect();
                self.length_input = filtered;
                if let Some(len) = parse_length_input(&self.length_input) {
                    if (1..=0x10000).contains(&len) {
                        self.display_length = len;
                    }
                }
                Task::none()
            }

            // ── Quick location ───────────────────────────────────
            MemoryEditorMessage::LocationSelected(location) => {
                self.selected_location = Some(location.clone());
                self.current_address = location.address as u32;
                self.display_length = location.length;
                self.address_input = format!("{:04X}", location.address);
                self.length_input = location.length.to_string();
                self.address_space = AddressSpace::C64Ram;
                self.status_message = Some(format!("Selected: {}", location.description));
                if connection.is_some() {
                    return self.update(
                        MemoryEditorMessage::ReadMemory,
                        connection,
                        host,
                        password,
                    );
                }
                Task::none()
            }

            // ── Read ─────────────────────────────────────────────
            MemoryEditorMessage::ReadMemory => match self.address_space {
                AddressSpace::C64Ram => {
                    if let Some(conn) = connection {
                        self.is_loading = true;
                        self.status_message = Some("Reading memory…".to_string());
                        let address = self.current_address as u16;
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
                AddressSpace::Reu => {
                    self.status_message =
                        Some("REU read not supported — write only via raw socket".to_string());
                    Task::none()
                }
            },

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
                    Err(e) => self.status_message = Some(format!("Read failed: {}", e)),
                }
                Task::none()
            }

            // ── Watch ────────────────────────────────────────────
            MemoryEditorMessage::ToggleWatch => {
                self.watch_active = !self.watch_active;
                if self.watch_active {
                    self.status_message =
                        Some("Watch mode ON — refreshing every 500 ms".to_string());
                } else {
                    self.status_message = Some("Watch mode OFF".to_string());
                }
                Task::none()
            }

            MemoryEditorMessage::WatchTick => {
                if self.watch_active && !self.is_loading {
                    // Silently re-read without updating status_message so it doesn't flicker
                    if let Some(conn) = connection {
                        let address = self.current_address as u16;
                        let length = self.display_length;
                        self.is_loading = true;
                        return Task::perform(
                            async move { read_memory_async(conn, address, length).await },
                            MemoryEditorMessage::ReadMemoryComplete,
                        );
                    }
                }
                Task::none()
            }

            // ── Byte editing ─────────────────────────────────────
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
                        let address =
                            (self.current_address as u16).wrapping_add(edit.offset as u16);
                        let offset = edit.offset;
                        let new_value = value;
                        self.is_loading = true;
                        self.status_message =
                            Some(format!("Writing ${:02X} to ${:04X}…", value, address));
                        return Task::perform(
                            async move {
                                write_byte_async(conn, address, new_value)
                                    .await
                                    .map(|_| (offset, new_value))
                            },
                            |result| match result {
                                Ok((off, val)) => {
                                    MemoryEditorMessage::WriteByteComplete(Ok((off, val)))
                                }
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
                    Ok((offset, new_value)) => {
                        if let (Some(data), Some(_edit)) =
                            (&mut self.memory_data, &self.editing_byte)
                        {
                            if offset < data.len() {
                                let old_value = data[offset];
                                let address =
                                    (self.current_address as u16).wrapping_add(offset as u16);
                                // Push undo entry
                                self.undo_stack.push(UndoEntry {
                                    address,
                                    old_value,
                                    new_value,
                                    offset,
                                });
                                self.redo_stack.clear(); // new write invalidates redo
                                data[offset] = new_value;
                                self.status_message =
                                    Some(format!("Written ${:02X} to ${:04X}", new_value, address));
                            }
                        }
                    }
                    Err(e) => self.status_message = Some(format!("Write failed: {}", e)),
                }
                self.editing_byte = None;
                Task::none()
            }

            // ── Undo ─────────────────────────────────────────────
            MemoryEditorMessage::Undo => {
                if let (Some(entry), Some(conn)) = (self.undo_stack.pop(), connection) {
                    let address = entry.address;
                    let restore_value = entry.old_value;
                    self.redo_stack.push(entry.clone());
                    // Patch local copy immediately for snappy UI
                    if let Some(data) = &mut self.memory_data {
                        if entry.offset < data.len() {
                            data[entry.offset] = restore_value;
                        }
                    }
                    self.status_message = Some(format!(
                        "Undo: restored ${:02X} at ${:04X}",
                        restore_value, address
                    ));
                    self.is_loading = true;
                    return Task::perform(
                        async move {
                            write_byte_async(conn, address, restore_value)
                                .await
                                .map(|_| ())
                        },
                        |r| match r {
                            Ok(()) => MemoryEditorMessage::DmaWriteComplete(Ok(())),
                            Err(e) => MemoryEditorMessage::DmaWriteComplete(Err(e)),
                        },
                    );
                }
                Task::none()
            }

            MemoryEditorMessage::Redo => {
                if let (Some(entry), Some(conn)) = (self.redo_stack.pop(), connection) {
                    let address = entry.address;
                    let new_value = entry.new_value;
                    self.undo_stack.push(entry.clone());
                    if let Some(data) = &mut self.memory_data {
                        if entry.offset < data.len() {
                            data[entry.offset] = new_value;
                        }
                    }
                    self.status_message = Some(format!(
                        "Redo: wrote ${:02X} to ${:04X}",
                        new_value, address
                    ));
                    self.is_loading = true;
                    return Task::perform(
                        async move { write_byte_async(conn, address, new_value).await.map(|_| ()) },
                        |r| match r {
                            Ok(()) => MemoryEditorMessage::DmaWriteComplete(Ok(())),
                            Err(e) => MemoryEditorMessage::DmaWriteComplete(Err(e)),
                        },
                    );
                }
                Task::none()
            }

            // ── Fill ─────────────────────────────────────────────
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
                        let address = self.current_address as u16;
                        let length = self.display_length;
                        self.status_message =
                            Some(format!("Filling {} bytes with ${:02X}…", length, value));
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
                        return self.update(
                            MemoryEditorMessage::ReadMemory,
                            connection,
                            host,
                            password,
                        );
                    }
                    Err(e) => self.status_message = Some(format!("Fill failed: {}", e)),
                }
                Task::none()
            }

            // ── Display mode / search ────────────────────────────
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
                        self.status_message = if matches.is_empty() {
                            Some("Pattern not found".to_string())
                        } else {
                            Some(format!("Found {} match(es)", matches.len()))
                        };
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
                self.watch_active = false;
                Task::none()
            }

            MemoryEditorMessage::RefreshMemory => {
                if self.memory_data.is_some() {
                    return self.update(
                        MemoryEditorMessage::ReadMemory,
                        connection,
                        host,
                        password,
                    );
                }
                Task::none()
            }

            // ── Save / Load dump ─────────────────────────────────
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
                        .wrapping_add(self.display_length.saturating_sub(1) as u32)
                );
                Task::perform(
                    async move {
                        rfd::AsyncFileDialog::new()
                            .set_file_name(&default_name)
                            .add_filter("Binary dump", &["bin"])
                            .add_filter("All files", &["*"])
                            .save_file()
                            .await
                            .map(|h| h.path().to_path_buf())
                    },
                    MemoryEditorMessage::SaveDumpPathSelected,
                )
            }

            MemoryEditorMessage::SaveDumpPathSelected(path) => {
                if let (Some(path), Some(data)) = (path, &self.memory_data) {
                    let data = data.clone();
                    return Task::perform(
                        async move {
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
                Task::none()
            }

            MemoryEditorMessage::SaveDumpComplete(result) => {
                self.status_message = Some(result.unwrap_or_else(|e| e));
                Task::none()
            }

            MemoryEditorMessage::LoadDump => Task::perform(
                async {
                    rfd::AsyncFileDialog::new()
                        .add_filter("Binary dump", &["bin"])
                        .add_filter("All files", &["*"])
                        .pick_file()
                        .await
                        .map(|h| h.path().to_path_buf())
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
                            "Loaded {} bytes — click 'Write to Device' to write to ${:04X}",
                            len, self.current_address
                        ));
                    }
                    Err(e) => self.status_message = Some(e),
                }
                Task::none()
            }

            MemoryEditorMessage::WriteDumpToDevice => {
                if let (Some(conn), Some(data)) = (connection, self.pending_load_data.take()) {
                    self.is_loading = true;
                    let address = self.current_address as u16;
                    let len = data.len();
                    self.status_message =
                        Some(format!("Writing {} bytes to ${:04X}…", len, address));
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
                        return self.update(
                            MemoryEditorMessage::ReadMemory,
                            connection,
                            host,
                            password,
                        );
                    }
                    Err(e) => self.status_message = Some(format!("Write failed: {}", e)),
                }
                Task::none()
            }

            // ── Bookmarks ────────────────────────────────────────
            MemoryEditorMessage::BookmarkLabelChanged(label) => {
                self.bookmark_label_input = label;
                Task::none()
            }

            MemoryEditorMessage::AddBookmark => {
                let label = if self.bookmark_label_input.trim().is_empty() {
                    format!("${:04X}", self.current_address)
                } else {
                    self.bookmark_label_input.trim().to_string()
                };
                let space = match self.address_space {
                    AddressSpace::C64Ram => BookmarkSpace::C64Ram,
                    AddressSpace::Reu => BookmarkSpace::Reu,
                };
                self.bookmarks.push(Bookmark {
                    label,
                    address: self.current_address,
                    length: self.display_length,
                    space,
                });
                self.bookmark_label_input.clear();
                self.status_message =
                    Some(format!("Bookmark added ({} total)", self.bookmarks.len()));
                Task::none()
            }

            MemoryEditorMessage::BookmarkSelected(bm) => {
                self.address_space = match bm.space {
                    BookmarkSpace::C64Ram => AddressSpace::C64Ram,
                    BookmarkSpace::Reu => AddressSpace::Reu,
                };
                self.current_address = bm.address;
                self.display_length = bm.length;
                let max_digits = if self.address_space == AddressSpace::Reu {
                    6
                } else {
                    4
                };
                self.address_input = format!("{:0width$X}", bm.address, width = max_digits);
                self.length_input = bm.length.to_string();
                self.status_message = Some(format!("Jumped to bookmark: {}", bm.label));
                if connection.is_some() && self.address_space == AddressSpace::C64Ram {
                    return self.update(
                        MemoryEditorMessage::ReadMemory,
                        connection,
                        host,
                        password,
                    );
                }
                Task::none()
            }

            MemoryEditorMessage::DeleteBookmark(idx) => {
                if idx < self.bookmarks.len() {
                    let name = self.bookmarks.remove(idx).label;
                    self.status_message = Some(format!("Bookmark '{}' removed", name));
                }
                Task::none()
            }

            // ─────────────────────────────────────────────────────
            //  Raw socket DMA commands
            // ─────────────────────────────────────────────────────
            MemoryEditorMessage::DmaWrite {
                host,
                password,
                offset,
                data,
            } => {
                self.is_loading = true;
                self.status_message = Some(format!(
                    "DMA-writing {} bytes to ${:04X}…",
                    data.len(),
                    offset
                ));
                Task::perform(
                    async move { port64::write_dma(host, password, offset, data).await },
                    MemoryEditorMessage::DmaWriteComplete,
                )
            }

            MemoryEditorMessage::DmaWriteComplete(result) => {
                self.is_loading = false;
                match &result {
                    Ok(()) => self.status_message = Some("DMA write complete".to_string()),
                    Err(e) => self.status_message = Some(format!("DMA write failed: {}", e)),
                }
                Task::none()
            }

            MemoryEditorMessage::DmaJump {
                host,
                password,
                offset,
                data,
            } => {
                self.is_loading = true;
                self.status_message = Some(format!(
                    "DMA-jump: loading {} bytes, jumping to ${:04X}…",
                    data.len(),
                    offset
                ));
                Task::perform(
                    async move { port64::write_dma_jump(host, password, offset, data).await },
                    MemoryEditorMessage::DmaJumpComplete,
                )
            }

            MemoryEditorMessage::DmaJumpComplete(result) => {
                self.is_loading = false;
                match &result {
                    Ok(()) => self.status_message = Some("DMA jump dispatched".to_string()),
                    Err(e) => self.status_message = Some(format!("DMA jump failed: {}", e)),
                }
                Task::none()
            }

            MemoryEditorMessage::ReuWrite {
                host,
                password,
                reu_offset,
                data,
            } => {
                self.is_loading = true;
                self.status_message = Some(format!(
                    "Writing {} bytes to REU ${:06X}…",
                    data.len(),
                    reu_offset
                ));
                Task::perform(
                    async move { port64::write_reu(host, password, reu_offset, data).await },
                    MemoryEditorMessage::ReuWriteComplete,
                )
            }

            MemoryEditorMessage::ReuWriteComplete(result) => {
                self.is_loading = false;
                match &result {
                    Ok(()) => self.status_message = Some("REU write complete".to_string()),
                    Err(e) => self.status_message = Some(format!("REU write failed: {}", e)),
                }
                Task::none()
            }

            // ── Kernal write ─────────────────────────────────────
            MemoryEditorMessage::KernalWriteClicked => Task::perform(
                async {
                    rfd::AsyncFileDialog::new()
                        .add_filter("Kernal ROM", &["bin", "rom"])
                        .add_filter("All files", &["*"])
                        .set_title("Select Kernal ROM image")
                        .pick_file()
                        .await
                        .map(|h| h.path().to_path_buf())
                },
                MemoryEditorMessage::KernalWritePathSelected,
            ),

            MemoryEditorMessage::KernalWritePathSelected(path_opt) => {
                if let (Some(path), Some(h), Some(pw)) =
                    (path_opt, host.clone(), Some(password.clone()))
                {
                    self.kernal_pending_path = Some(path.clone());
                    self.is_loading = true;
                    self.status_message = Some("Sending Kernal ROM image…".to_string());
                    return Task::perform(
                        async move {
                            let data = std::fs::read(&path)
                                .map_err(|e| format!("Read ROM failed: {}", e))?;
                            port64::write_kernal(h, pw, data).await
                        },
                        MemoryEditorMessage::KernalWriteComplete,
                    );
                }
                Task::none()
            }

            MemoryEditorMessage::KernalWriteComplete(result) => {
                self.is_loading = false;
                match &result {
                    Ok(()) => {
                        self.status_message = Some("Kernal ROM replaced successfully".to_string())
                    }
                    Err(e) => self.status_message = Some(format!("Kernal write failed: {}", e)),
                }
                Task::none()
            }

            // ── Flash inspector ──────────────────────────────────
            MemoryEditorMessage::ReadFlashInfo => {
                if let Some(h) = host.clone() {
                    self.is_loading = true;
                    self.status_message = Some("Reading flash info…".to_string());
                    return Task::perform(
                        async move { port64::flash_info(h, password).await },
                        MemoryEditorMessage::FlashInfoComplete,
                    );
                }
                Task::none()
            }

            MemoryEditorMessage::FlashInfoComplete(result) => {
                self.is_loading = false;
                match result {
                    Ok((page_size, page_count)) => {
                        self.flash_info = Some(FlashInfo {
                            page_size,
                            page_count,
                            pages: Vec::new(),
                            current_page: 0,
                        });
                        self.status_message = Some(format!(
                            "Flash: {} pages × {} bytes = {} KB",
                            page_count,
                            page_size,
                            (page_count as u64 * page_size as u64) / 1024
                        ));
                    }
                    Err(e) => self.status_message = Some(format!("Flash info failed: {}", e)),
                }
                Task::none()
            }

            MemoryEditorMessage::FlashPageChanged(value) => {
                self.flash_page_input = value.chars().filter(|c| c.is_ascii_digit()).collect();
                Task::none()
            }

            MemoryEditorMessage::ReadFlashPage(page) => {
                if let Some(h) = host.clone() {
                    self.is_loading = true;
                    self.status_message = Some(format!("Reading flash page {}…", page));
                    if let Some(fi) = &mut self.flash_info {
                        fi.current_page = page;
                    }
                    return Task::perform(
                        async move { port64::flash_page(h, password, page).await },
                        MemoryEditorMessage::FlashPageComplete,
                    );
                }
                Task::none()
            }

            MemoryEditorMessage::FlashPageComplete(result) => {
                self.is_loading = false;
                match result {
                    Ok((page, data)) => {
                        if let Some(fi) = &mut self.flash_info {
                            // Store/replace page
                            if page as usize >= fi.pages.len() {
                                fi.pages.resize(page as usize + 1, Vec::new());
                            }
                            let len = data.len();
                            fi.pages[page as usize] = data;
                            self.status_message =
                                Some(format!("Flash page {} read ({} bytes)", page, len));
                        }
                    }
                    Err(e) => self.status_message = Some(format!("Flash read failed: {}", e)),
                }
                Task::none()
            }
        }
    }

    // ── Search ───────────────────────────────────────────────────

    fn perform_search(&self, data: &[u8]) -> Vec<usize> {
        let search_text = self.search_input.trim().to_lowercase();
        let mut matches = Vec::new();

        let is_hex = search_text
            .chars()
            .all(|c| c.is_ascii_hexdigit() || c.is_whitespace());

        if is_hex && search_text.len() >= 2 {
            let hex_clean: String = search_text.chars().filter(|c| !c.is_whitespace()).collect();
            if hex_clean.len() % 2 == 0 {
                let mut search_bytes = Vec::new();
                for chunk in hex_clean.as_bytes().chunks(2) {
                    if let Ok(s) = std::str::from_utf8(chunk) {
                        if let Ok(b) = u8::from_str_radix(s, 16) {
                            search_bytes.push(b);
                        }
                    }
                }
                if !search_bytes.is_empty() {
                    for i in 0..=data.len().saturating_sub(search_bytes.len()) {
                        if data[i..].starts_with(&search_bytes) {
                            matches.push(i);
                        }
                    }
                    return matches;
                }
            }
        }

        let search_bytes = search_text.as_bytes();
        if !search_bytes.is_empty() {
            for i in 0..=data.len().saturating_sub(search_bytes.len()) {
                let window = &data[i..i + search_bytes.len()];
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

    // ─────────────────────────────────────────────────────────────
    //  View
    // ─────────────────────────────────────────────────────────────

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
                } else if self.flash_info.is_some() {
                    self.view_flash_inspector(font_size)
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

    // ── Controls bar ─────────────────────────────────────────────

    fn view_controls(&self, font_size: u32) -> Element<'_, MemoryEditorMessage> {
        let fs = crate::styles::FontSizes::from_base(font_size);

        // Address space picker
        let space_picker = pick_list(
            vec![AddressSpace::C64Ram, AddressSpace::Reu],
            Some(self.address_space),
            MemoryEditorMessage::AddressSpaceChanged,
        )
        .text_size(fs.small)
        .width(Length::Fixed(90.0));

        // Address input
        let addr_prefix = if self.address_space == AddressSpace::Reu {
            "REU: $"
        } else {
            "Address: $"
        };
        let address_row = row![
            text(addr_prefix).size(fs.small),
            text_input("0400", &self.address_input)
                .on_input(MemoryEditorMessage::AddressInputChanged)
                .width(Length::Fixed(70.0))
                .size(fs.small),
        ]
        .spacing(5)
        .align_y(iced::Alignment::Center);

        let length_row = row![
            text("Length:").size(fs.small),
            text_input("256", &self.length_input)
                .on_input(MemoryEditorMessage::LengthInputChanged)
                .width(Length::Fixed(60.0))
                .size(fs.small),
            text("bytes").size(fs.small),
        ]
        .spacing(5)
        .align_y(iced::Alignment::Center);

        let read_btn_inner = button(text("Read").size(fs.small))
            .on_press_maybe(
                if self.is_loading || self.address_space == AddressSpace::Reu {
                    None
                } else {
                    Some(MemoryEditorMessage::ReadMemory)
                },
            )
            .padding([5, 15]);

        let read_btn: Element<'_, MemoryEditorMessage> = if self.address_space == AddressSpace::Reu
        {
            tooltip(
                read_btn_inner,
                container(text("REU read not supported by firmware").size(fs.small))
                    .padding(6)
                    .style(tooltip_style),
                tooltip::Position::Bottom,
            )
            .into()
        } else {
            read_btn_inner.into()
        };

        let location_picker = pick_list(
            MEMORY_LOCATIONS.to_vec(),
            self.selected_location.clone(),
            MemoryEditorMessage::LocationSelected,
        )
        .placeholder("Quick Locations…")
        .width(Length::Fixed(200.0))
        .text_size(fs.small);

        let first_row = row![
            space_picker,
            Space::new().width(Length::Fixed(10.0)),
            address_row,
            Space::new().width(Length::Fixed(10.0)),
            length_row,
            Space::new().width(Length::Fixed(10.0)),
            read_btn,
            Space::new().width(Length::Fixed(10.0)),
            location_picker,
        ]
        .spacing(10)
        .align_y(iced::Alignment::Center);

        // Search + display mode row
        let search_row = row![
            text("Search:").size(fs.small),
            text_input("Hex or ASCII", &self.search_input)
                .on_input(MemoryEditorMessage::SearchInputChanged)
                .on_submit(MemoryEditorMessage::PerformSearch)
                .width(Length::Fixed(150.0))
                .size(fs.small),
            button(text("Find").size(fs.small))
                .on_press_maybe(
                    if self.search_input.is_empty() || self.memory_data.is_none() {
                        None
                    } else {
                        Some(MemoryEditorMessage::PerformSearch)
                    }
                )
                .padding([5, 10]),
            button(text("Clear").size(fs.small))
                .on_press(MemoryEditorMessage::ClearSearch)
                .padding([5, 10]),
            Space::new().width(Length::Fixed(20.0)),
            text("Display:").size(fs.small),
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
            .text_size(fs.small)
            .width(Length::Fixed(80.0)),
        ]
        .spacing(10)
        .align_y(iced::Alignment::Center);

        // Fill + save/load row
        let fill_row = row![
            text("Fill: $").size(fs.small),
            text_input("00", &self.fill_value_input)
                .on_input(MemoryEditorMessage::FillValueChanged)
                .width(Length::Fixed(40.0))
                .size(fs.small),
            button(text("Fill Range").size(fs.small))
                .on_press_maybe(if self.is_loading {
                    None
                } else {
                    Some(MemoryEditorMessage::FillMemory)
                })
                .padding([5, 10]),
            Space::new().width(Length::Fixed(15.0)),
            button(text("Save Dump…").size(fs.small))
                .on_press_maybe(if self.is_loading || self.memory_data.is_none() {
                    None
                } else {
                    Some(MemoryEditorMessage::SaveDump)
                })
                .padding([5, 10]),
            button(text("Load Dump…").size(fs.small))
                .on_press_maybe(if self.is_loading {
                    None
                } else {
                    Some(MemoryEditorMessage::LoadDump)
                })
                .padding([5, 10]),
        ]
        .spacing(10)
        .align_y(iced::Alignment::Center);

        // Undo / Redo / Watch row
        let undo_row = row![
            button(text("⟲ Undo").size(fs.small))
                .on_press_maybe(if self.undo_stack.is_empty() {
                    None
                } else {
                    Some(MemoryEditorMessage::Undo)
                })
                .padding([5, 10]),
            button(text("⟳ Redo").size(fs.small))
                .on_press_maybe(if self.redo_stack.is_empty() {
                    None
                } else {
                    Some(MemoryEditorMessage::Redo)
                })
                .padding([5, 10]),
            text(format!(
                "({} / {})",
                self.undo_stack.len(),
                self.redo_stack.len()
            ))
            .size(fs.small),
            Space::new().width(Length::Fixed(20.0)),
            button(
                text(if self.watch_active {
                    "⏹ Stop Watch"
                } else {
                    "👁 Watch"
                })
                .size(fs.small)
            )
            .on_press_maybe(if self.memory_data.is_none() {
                None
            } else {
                Some(MemoryEditorMessage::ToggleWatch)
            })
            .style(if self.watch_active {
                button::primary
            } else {
                button::secondary
            })
            .padding([5, 10]),
            Space::new().width(Length::Fixed(20.0)),
            // Raw-socket DMA tools
            button(text("Write ROM…").size(fs.small))
                .on_press(MemoryEditorMessage::KernalWriteClicked)
                .padding([5, 10]),
            button(
                text(if self.flash_info.is_some() {
                    "Flash ✓"
                } else {
                    "Flash Info"
                })
                .size(fs.small)
            )
            .on_press(MemoryEditorMessage::ReadFlashInfo)
            .padding([5, 10]),
        ]
        .spacing(10)
        .align_y(iced::Alignment::Center);

        // Bookmark row
        let bm_row = self.view_bookmark_bar(fs.small);

        // Status / write-to-device row
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
                .size(fs.small)
                .color(iced::Color::from_rgb(0.3, 0.8, 0.3)),
                Space::new().width(Length::Fixed(10.0)),
                button(text("Write to Device").size(fs.small))
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
                    text(msg).size(fs.small)
                } else {
                    text("").size(fs.small)
                },
                Space::new().width(Length::Fill),
            ]
            .spacing(10)
            .align_y(iced::Alignment::Center)
            .into()
        };

        column![first_row, search_row, fill_row, undo_row, bm_row, status_row]
            .spacing(8)
            .into()
    }

    // ── Bookmark bar ─────────────────────────────────────────────

    fn view_bookmark_bar(&self, sf: u32) -> Element<'_, MemoryEditorMessage> {
        let mut bm_row = Row::new()
            .spacing(8)
            .align_y(iced::Alignment::Center)
            .push(text("Bookmarks:").size(sf));

        for (i, bm) in self.bookmarks.iter().enumerate() {
            bm_row = bm_row.push(
                button(text(&*bm.label).size(sf.saturating_sub(1)))
                    .on_press(MemoryEditorMessage::BookmarkSelected(bm.clone()))
                    .style(button::secondary)
                    .padding([3, 8]),
            );
            bm_row = bm_row.push(
                button(text("✕").size(sf.saturating_sub(2)))
                    .on_press(MemoryEditorMessage::DeleteBookmark(i))
                    .style(button::danger)
                    .padding([3, 5]),
            );
        }

        bm_row = bm_row.push(
            text_input("Label…", &self.bookmark_label_input)
                .on_input(MemoryEditorMessage::BookmarkLabelChanged)
                .on_submit(MemoryEditorMessage::AddBookmark)
                .width(Length::Fixed(120.0))
                .size(sf),
        );
        bm_row = bm_row.push(
            button(text("+ Add").size(sf))
                .on_press(MemoryEditorMessage::AddBookmark)
                .padding([3, 8]),
        );

        bm_row.into()
    }

    // ── Memory hex display ───────────────────────────────────────

    fn view_memory_display(&self, font_size: u32) -> Element<'_, MemoryEditorMessage> {
        let fs = crate::styles::FontSizes::from_base(font_size);

        let Some(data) = &self.memory_data else {
            return Space::new().width(Length::Fill).height(Length::Fill).into();
        };

        let watch_label = if self.watch_active { " 👁 LIVE" } else { "" };
        let header = row![
            text(format!(
                "Memory at ${:04X} — {} bytes{}",
                self.current_address,
                data.len(),
                watch_label
            ))
            .size(fs.small),
            Space::new().width(Length::Fill),
            button(text("Refresh").size(fs.small))
                .on_press(MemoryEditorMessage::RefreshMemory)
                .padding([5, 10]),
            button(text("Close").size(fs.small))
                .on_press(MemoryEditorMessage::ClearMemoryView)
                .padding([5, 10]),
        ]
        .spacing(10)
        .align_y(iced::Alignment::Center);

        let mut rows: Vec<Element<'_, MemoryEditorMessage>> = Vec::new();

        // Column headers
        let mut hdr_row = Row::new().push(text("ADDR").size(fs.tiny).width(Length::Fixed(50.0)));
        for i in 0..16u8 {
            hdr_row = hdr_row.push(
                text(format!("{:X}", i))
                    .size(fs.tiny)
                    .width(Length::Fixed(24.0)),
            );
        }
        hdr_row = hdr_row
            .push(Space::new().width(Length::Fixed(10.0)))
            .push(text("ASCII").size(fs.tiny));
        rows.push(hdr_row.spacing(2).into());

        // Data rows
        for (row_idx, chunk) in data.chunks(16).enumerate() {
            let row_addr = (self.current_address as u16).wrapping_add((row_idx * 16) as u16);
            let row_base = row_idx * 16;

            let mut data_row = Row::new().push(
                text(format!("{:04X}", row_addr))
                    .size(fs.tiny)
                    .width(Length::Fixed(50.0))
                    .color(iced::Color::from_rgb(0.4, 0.5, 0.9)),
            );

            for (bi, &byte) in chunk.iter().enumerate() {
                let offset = row_base + bi;
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
                    container(
                        text(byte_text.clone())
                            .size(fs.tiny)
                            .color(iced::Color::BLACK),
                    )
                    .style(editing_style)
                    .width(Length::Fixed(width))
                } else if is_match {
                    container(
                        text(byte_text.clone())
                            .size(fs.tiny)
                            .color(iced::Color::BLACK),
                    )
                    .style(highlight_style)
                    .width(Length::Fixed(width))
                } else {
                    container(text(byte_text.clone()).size(fs.tiny)).width(Length::Fixed(width))
                };

                let tip = format!(
                    "${:04X}: {} ({})",
                    (self.current_address as u16).wrapping_add(offset as u16),
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
                    container(text(tip).size(fs.small))
                        .padding(6)
                        .style(tooltip_style),
                    tooltip::Position::Bottom,
                ));
            }

            for _ in chunk.len()..16 {
                data_row = data_row.push(Space::new().width(Length::Fixed(24.0)));
            }

            data_row = data_row.push(Space::new().width(Length::Fixed(10.0)));
            let ascii: String = chunk
                .iter()
                .map(|&b| if b >= 32 && b <= 126 { b as char } else { '.' })
                .collect();
            data_row = data_row.push(
                text(ascii)
                    .size(fs.tiny)
                    .color(iced::Color::from_rgb(0.6, 0.6, 0.6)),
            );

            rows.push(data_row.spacing(2).into());
        }

        let mem_scroll: Element<'_, MemoryEditorMessage> =
            scrollable(Column::with_children(rows).spacing(1))
                .height(Length::Fill)
                .into();

        // Optional byte-edit dialog
        if let Some(edit) = &self.editing_byte {
            let address = (self.current_address as u16).wrapping_add(edit.offset as u16);
            let dialog = container(
                column![
                    text(format!("Edit byte at ${:04X}", address)).size(font_size),
                    rule::horizontal(1),
                    row![
                        text("Current:").size(fs.small),
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
                        .size(fs.small),
                    ]
                    .spacing(10),
                    row![
                        text("New value (0–255):").size(fs.small),
                        text_input("0", &edit.new_value_input)
                            .on_input(MemoryEditorMessage::WriteByteValueChanged)
                            .on_submit(MemoryEditorMessage::WriteByteConfirm)
                            .width(Length::Fixed(80.0))
                            .size(fs.small),
                    ]
                    .spacing(10),
                    row![
                        button(text("Write").size(fs.small))
                            .on_press(MemoryEditorMessage::WriteByteConfirm)
                            .padding([5, 15]),
                        button(text("Cancel").size(fs.small))
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
                    mem_scroll,
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
            column![header, rule::horizontal(1), mem_scroll]
                .spacing(5)
                .into()
        }
    }

    // ── Flash inspector view ──────────────────────────────────────

    fn view_flash_inspector(&self, font_size: u32) -> Element<'_, MemoryEditorMessage> {
        let fs = crate::styles::FontSizes::from_base(font_size);

        let Some(fi) = &self.flash_info else {
            return Space::new().into();
        };

        let info_bar = row![
            text(format!(
                "Flash: {} pages × {} bytes ({} KB total)",
                fi.page_count,
                fi.page_size,
                (fi.page_count as u64 * fi.page_size as u64) / 1024
            ))
            .size(fs.small),
            Space::new().width(Length::Fill),
            button(text("Close Flash").size(fs.small))
                .on_press(MemoryEditorMessage::ClearMemoryView)
                .padding([5, 10]),
        ]
        .spacing(10)
        .align_y(iced::Alignment::Center);

        let page_controls = row![
            text("Page:").size(fs.small),
            text_input("0", &self.flash_page_input)
                .on_input(MemoryEditorMessage::FlashPageChanged)
                .width(Length::Fixed(60.0))
                .size(fs.small),
            button(text("Read Page").size(fs.small))
                .on_press_maybe(
                    self.flash_page_input
                        .parse::<u32>()
                        .ok()
                        .filter(|&p| p < fi.page_count && !self.is_loading)
                        .map(MemoryEditorMessage::ReadFlashPage)
                )
                .padding([5, 10]),
        ]
        .spacing(10)
        .align_y(iced::Alignment::Center);

        // Show current page data if available
        let page_display: Element<'_, MemoryEditorMessage> =
            if let Ok(pg) = self.flash_page_input.parse::<usize>() {
                if let Some(page_data) = fi.pages.get(pg).filter(|d| !d.is_empty()) {
                    let mut rows: Vec<Element<'_, MemoryEditorMessage>> = Vec::new();
                    for (ri, chunk) in page_data.chunks(16).enumerate() {
                        let row_addr = (ri * 16) as u32;
                        let mut dr = Row::new().push(
                            text(format!("{:06X}", row_addr))
                                .size(fs.tiny)
                                .width(Length::Fixed(60.0))
                                .color(iced::Color::from_rgb(0.5, 0.6, 0.9)),
                        );
                        for &b in chunk {
                            dr = dr.push(
                                text(format!("{:02X}", b))
                                    .size(fs.tiny)
                                    .width(Length::Fixed(24.0)),
                            );
                        }
                        rows.push(dr.spacing(2).into());
                    }
                    scrollable(Column::with_children(rows).spacing(1))
                        .height(Length::Fill)
                        .into()
                } else {
                    text(format!("Page {} not yet fetched — click 'Read Page'", pg))
                        .size(fs.small)
                        .into()
                }
            } else {
                text("Enter a page number above").size(fs.small).into()
            };

        column![
            info_bar,
            rule::horizontal(1),
            page_controls,
            rule::horizontal(1),
            page_display
        ]
        .spacing(8)
        .into()
    }

    // ── Quick locations grid ──────────────────────────────────────

    fn view_quick_locations(&self, font_size: u32) -> Element<'_, MemoryEditorMessage> {
        let fs = crate::styles::FontSizes::from_base(font_size);

        let title = text("Common C64 Memory Locations").size(fs.large);
        let subtitle = text("Click a location to view its contents")
            .size(fs.small)
            .color(iced::Color::from_rgb(0.6, 0.6, 0.6));

        let mut rows: Vec<Element<'_, MemoryEditorMessage>> = Vec::new();
        for chunk in MEMORY_LOCATIONS.chunks(3) {
            let mut row_items = Row::new().spacing(10);
            for location in chunk {
                let card = button(
                    container(
                        column![
                            text(location.name).size(fs.small).color(iced::Color::BLACK),
                            text(location.description)
                                .size(fs.tiny)
                                .color(iced::Color::from_rgb(0.3, 0.3, 0.3)),
                            row![
                                text(format!("${:04X}", location.address))
                                    .size(fs.small)
                                    .color(iced::Color::from_rgb(0.2, 0.3, 0.7)),
                                text(format!("{} bytes", location.length))
                                    .size(fs.tiny)
                                    .color(iced::Color::from_rgb(0.3, 0.3, 0.3)),
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
            for _ in chunk.len()..3 {
                row_items = row_items.push(Space::new().width(Length::Fill));
            }
            rows.push(row_items.width(Length::Fill).into());
        }

        scrollable(
            column![
                title,
                subtitle,
                Column::with_children(rows).spacing(10).width(Length::Fill),
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

// ─────────────────────────────────────────────────────────────────
//  Style helpers (delegated to crate::styles)
// ─────────────────────────────────────────────────────────────────

fn highlight_style(theme: &iced::Theme) -> container::Style {
    crate::styles::highlight_style(theme)
}

fn editing_style(theme: &iced::Theme) -> container::Style {
    crate::styles::editing_style(theme)
}

fn tooltip_style(theme: &iced::Theme) -> container::Style {
    crate::styles::tooltip_style(theme)
}

/// Parse a length entered by the user. Hex when prefixed with `$` / `0x` /
/// `0X`, suffixed with `h` / `H`, OR contains any a-f / A-F digit; decimal
/// otherwise. Trailing/leading whitespace is forgiven. Returns `None` for
/// empty / unparseable input so the caller can keep the previous value.
///
/// Examples:
///   `1024`  → Some(1024)        decimal
///   `$400`  → Some(0x400 = 1024)
///   `0xff`  → Some(0xFF)
///   `400h`  → Some(0x400)
///   `1FF`   → Some(0x1FF)        auto-hex (contains `F`)
///   ``      → None
fn parse_length_input(input: &str) -> Option<u32> {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return None;
    }
    let (digits, force_hex) = if let Some(r) = trimmed.strip_prefix('$') {
        (r, true)
    } else if let Some(r) = trimmed.strip_prefix("0x").or_else(|| trimmed.strip_prefix("0X")) {
        (r, true)
    } else if let Some(r) = trimmed.strip_suffix('h').or_else(|| trimmed.strip_suffix('H')) {
        (r, true)
    } else {
        (trimmed, false)
    };
    if digits.is_empty() {
        return None;
    }
    if force_hex || digits.chars().any(|c| c.is_ascii_hexdigit() && !c.is_ascii_digit()) {
        u32::from_str_radix(digits, 16).ok()
    } else {
        digits.parse::<u32>().ok()
    }
}

// ─────────────────────────────────────────────────────────────────
//  REST async helpers (delegated to crate::api)
// ─────────────────────────────────────────────────────────────────

async fn read_memory_async(
    connection: Arc<TokioMutex<Rest>>,
    address: u16,
    length: u32,
) -> Result<Vec<u8>, String> {
    crate::api::read_memory_async(connection, address, length).await
}

async fn write_byte_async(
    connection: Arc<TokioMutex<Rest>>,
    address: u16,
    value: u8,
) -> Result<(), String> {
    crate::api::write_byte_async(connection, address, value).await
}

async fn fill_memory_async(
    connection: Arc<TokioMutex<Rest>>,
    address: u16,
    length: u32,
    value: u8,
) -> Result<(), String> {
    crate::api::fill_memory_async(connection, address, length, value).await
}

async fn write_memory_async(
    connection: Arc<TokioMutex<Rest>>,
    address: u16,
    data: Vec<u8>,
) -> Result<(), String> {
    crate::api::write_memory_async(connection, address, data).await
}

#[cfg(test)]
mod tests {
    use super::parse_length_input;

    #[test]
    fn parse_decimal() {
        assert_eq!(parse_length_input("256"), Some(256));
        assert_eq!(parse_length_input("65536"), Some(65536));
        assert_eq!(parse_length_input("  100  "), Some(100));
    }

    #[test]
    fn parse_hex_with_dollar() {
        assert_eq!(parse_length_input("$100"), Some(0x100));
        assert_eq!(parse_length_input("$FF"), Some(0xFF));
        assert_eq!(parse_length_input("$10000"), Some(0x10000));
    }

    #[test]
    fn parse_hex_with_0x() {
        assert_eq!(parse_length_input("0x100"), Some(0x100));
        assert_eq!(parse_length_input("0XFF"), Some(0xFF));
    }

    #[test]
    fn parse_hex_with_suffix() {
        assert_eq!(parse_length_input("100h"), Some(0x100));
        assert_eq!(parse_length_input("FFh"), Some(0xFF));
        assert_eq!(parse_length_input("FFH"), Some(0xFF));
    }

    #[test]
    fn parse_auto_hex_when_letters_present() {
        // No prefix/suffix, but contains hex letters → treated as hex.
        assert_eq!(parse_length_input("FF"), Some(0xFF));
        assert_eq!(parse_length_input("1ff"), Some(0x1FF));
    }

    #[test]
    fn parse_invalid_returns_none() {
        assert_eq!(parse_length_input(""), None);
        assert_eq!(parse_length_input("$"), None);
        assert_eq!(parse_length_input("garbage"), None);
    }

    #[test]
    fn parse_full_64k_works() {
        // Regression for the bug where length 65536 couldn't fit in u16.
        assert_eq!(parse_length_input("65536"), Some(65536));
        assert_eq!(parse_length_input("$10000"), Some(0x10000));
        assert_eq!(parse_length_input("10000h"), Some(0x10000));
    }
}
