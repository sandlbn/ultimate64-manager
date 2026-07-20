// Remove the windows_subsystem attribute during development to see console output
// Uncomment for release builds:
#![cfg_attr(target_os = "windows", windows_subsystem = "windows")]

use iced::{
    widget::{button, column, container, row, rule, scrollable, text, text_input, Space},
    window, Element, Length, Subscription, Task, Theme,
};
use net_utils::REST_TIMEOUT_SECS;
use std::path::PathBuf;

/// Background status poll cadence when the device is reachable.
const STATUS_POLL_NORMAL_SECS: u64 = 60;
/// Background reconnect cadence when the app has officially disconnected
/// (consecutive failures exceeded [`MAX_TRANSIENT_STATUS_FAILURES`]) but the
/// user still has a host configured. A successful poll flips back to
/// "Connected" automatically — no user intervention needed.
const STATUS_POLL_RECONNECT_SECS: u64 = 10;
/// Faster poll cadence used during a transient outage window so a 2-3s
/// reboot is recovered well before the user sees "Not connected".
const STATUS_POLL_RETRY_SECS: u64 = 2;
/// How many consecutive failed polls we tolerate before flipping the UI to
/// "Not connected". With [`STATUS_POLL_RETRY_SECS`] = 2s, this gives ~6s of
/// grace before disconnect — comfortably covers a normal reboot.
const MAX_TRANSIENT_STATUS_FAILURES: u8 = 3;

/// Keyboard shortcuts rendered by the `?` Help overlay. Tuples are
/// `(section, key, description)` — grouping is by adjacent equal section
/// names so order matters. New keybinds should be added here too so the
/// help stays in sync with reality.
const HELP_BINDS: &[(&str, &str, &str)] = &[
    ("Navigation", "Tab", "Switch between local and remote pane"),
    ("Navigation", "↑ / ↓", "Move cursor up / down in file list"),
    (
        "Navigation",
        "Enter",
        "Open folder · Run PRG/CRT/SID · Mount disk image",
    ),
    ("Navigation", "Backspace", "Go to parent folder"),
    ("Navigation", "Cmd/Ctrl + ←", "Go to parent folder"),
    ("Navigation", "Cmd/Ctrl + ↑", "Go to parent folder"),
    ("File operations", "F2", "Rename selected file"),
    ("File operations", "F3", "View / preview selected file"),
    (
        "File operations",
        "F4",
        "Edit selected local file (OS default editor)",
    ),
    ("File operations", "F5", "Copy selected to other pane"),
    ("File operations", "F7", "Create new folder"),
    ("File operations", "F8", "Delete selected"),
    (
        "File operations",
        "Space",
        "Calculate folder size + toggle selection",
    ),
    (
        "Search & filter",
        "Type letters",
        "Quick-jump to first matching file",
    ),
    (
        "Search & filter",
        "Esc",
        "Clear quick-search / close dialogs",
    ),
    (
        "Search & filter",
        "Cmd/Ctrl + L",
        "Focus path field (type a path, Enter to navigate)",
    ),
    ("General", "Cmd/Ctrl + R", "Refresh active pane"),
    ("General", "Cmd/Ctrl + A", "Select all in active pane"),
    ("General", "?", "Show this help"),
    ("Memory editor", "Cmd/Ctrl + Z", "Undo last write"),
    ("Memory editor", "Cmd/Ctrl + Shift + Z", "Redo"),
    ("Video / streaming", "Alt/Opt + F", "Toggle fullscreen"),
    ("Video / streaming", "Esc", "Exit fullscreen"),
];

use std::sync::Arc;
use std::sync::Mutex;
use ultimate64::Rest;
use url::Host;
use version_check::{NewVersionInfo, VersionCheckMessage};

mod api;
mod app;
mod archive;
mod assembly64;
mod assembly64_browser;
mod basic_editor;
mod basic_tokenizer;
mod cfg_format;
mod config_api;
mod config_editor;
mod config_presets;
mod csdb_screenshots;
mod debug_stream;
mod device_profile;
mod dir_preview;
mod discovery;
mod disk_image;
mod file_browser;
mod file_types;
mod folder_favorites;
mod ftp_ops;
mod memory_editor;
mod mod_info;
mod music_ops;
mod music_player;
mod net_utils;
mod pdf_preview;
mod petscii;
mod port64;
mod profile_api;
mod profile_manager;
mod profile_repo;
mod profiles;
mod remote_browser;
mod screenshot_api;
mod settings;
mod sid_info;
mod sid_monitor;
mod stream_control;
mod streaming;
mod string_utils;
mod styles;
mod tab;
mod templates;
mod version_check;
mod video_scaling;

use assembly64_browser::{Assembly64Browser, Assembly64BrowserMessage};
use basic_editor::{BasicEditor, BasicEditorMessage};
use config_editor::{ConfigEditor, ConfigEditorMessage};
use discovery::DiscoveredDevice;
use file_browser::{FileBrowser, FileBrowserMessage};
use memory_editor::{MemoryEditor, MemoryEditorMessage};
use music_player::{MusicPlayer, MusicPlayerMessage, PlaybackState};
use profile_manager::{ProfileManager as DeviceProfileManager, ProfileManagerMessage};
use profiles::ProfileManager;
use remote_browser::{RemoteBrowser, RemoteBrowserMessage};
use settings::{AppSettings, StreamControlMethod};
use sid_monitor::{SidMonitor, SidMonitorMessage};
use streaming::{StreamingMessage, VideoStreaming};
use tab::{TabContext, TabController};
use templates::{DiskTemplate, TemplateManager};

const APP_VERSION: &str = env!("CARGO_PKG_VERSION");

pub fn main() -> iced::Result {
    // Force OpenGL backend on Linux for better compatibility with multi-GPU systems
    // Users can override with WGPU_BACKEND=vulkan if needed
    #[cfg(target_os = "linux")]
    if std::env::var("WGPU_BACKEND").is_err() {
        // SAFETY: This is executed at program startup before any threads
        // or GPU backends are initialized. No other threads can read
        // environment variables at this point.
        unsafe {
            std::env::set_var("WGPU_BACKEND", "gl");
        }
    }

    // Initialize logger - show info level by default, debug if RUST_LOG is set
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info"))
        .format_timestamp_secs()
        .init();

    log::info!("===========================================");
    log::info!("Starting Ultimate64 Manager v{}", APP_VERSION);
    log::info!("===========================================");

    // Print some diagnostic info
    log::info!("Platform: {}", std::env::consts::OS);
    log::info!("Arch: {}", std::env::consts::ARCH);

    // Install a panic hook that suppresses the harmless "SendError { kind: Disconnected }"
    // panic that occurs when background tasks (FTP transfers) try to send results back
    // after the window/event loop has been closed. This is an Iced framework limitation —
    // Task::perform futures can outlive the event loop on shutdown.
    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let msg = info.to_string();
        if msg.contains("SendError") && msg.contains("Disconnected") {
            // Silently ignore — app is shutting down, this is expected
            log::debug!("Suppressed shutdown panic: {}", msg);
            std::process::exit(0);
        }
        default_hook(info);
    }));

    iced::daemon(
        Ultimate64Browser::new,
        Ultimate64Browser::update,
        Ultimate64Browser::view,
    )
    .title(Ultimate64Browser::title)
    .subscription(Ultimate64Browser::subscription)
    .theme(Ultimate64Browser::theme)
    .run()
}

fn load_window_icon() -> Option<iced::window::Icon> {
    // Embedded icon - icon.png MUST exist in icons/ folder at compile time
    // If you don't have icons/icon.png, compilation will fail
    const ICON_BYTES: &[u8] = include_bytes!("../icons/icon.png");

    if let Ok(img) = image::load_from_memory(ICON_BYTES) {
        let img = img.resize_exact(32, 32, image::imageops::FilterType::Lanczos3);
        let rgba = img.to_rgba8();
        let (width, height) = rgba.dimensions();
        if let Ok(icon) = iced::window::icon::from_rgba(rgba.into_raw(), width, height) {
            log::info!("Using embedded icon (32x32)");
            return Some(icon);
        }
    }

    log::warn!("Failed to load embedded icon, using fallback");

    // Fallback: create a simple colored icon programmatically
    let size = 32u32;
    let mut pixels = Vec::with_capacity((size * size * 4) as usize);
    for y in 0..size {
        for x in 0..size {
            let border = x < 2 || x >= size - 2 || y < 2 || y >= size - 2;
            if border {
                pixels.extend_from_slice(&[134, 122, 222, 255]); // C64 light blue
            } else {
                pixels.extend_from_slice(&[64, 50, 133, 255]); // C64 blue
            }
        }
    }
    iced::window::icon::from_rgba(pixels, size, size).ok()
}

#[derive(Debug, Clone)]
pub enum Message {
    // Navigation
    TabSelected(Tab),

    // File browsers (dual pane)
    LeftBrowser(FileBrowserMessage),
    RemoteBrowser(RemoteBrowserMessage),
    ActivePaneChanged(Pane),

    // Copy operations
    CopyLocalToRemote, // Copy selected local file to Ultimate64
    CopyRemoteToLocal, // Copy selected remote file to local
    CopyComplete(Result<String, String>),
    CopyCancel,
    CopyProgressTick,
    CopyOverwriteConfirm, // User confirmed overwriting existing remote files
    CopyOverwriteCancel,  // User cancelled the overwrite prompt

    // Music player
    MusicPlayer(MusicPlayerMessage),

    // Configuration editor
    ConfigEditor(ConfigEditorMessage),

    // Device profile manager
    DeviceProfileManager(ProfileManagerMessage),

    // Connection
    HostInputChanged(String),
    PasswordInputChanged(String),
    ConnectPressed,
    DisconnectPressed,
    RefreshStatus,
    RefreshAfterConnect,
    StatusUpdated(Result<StatusInfo, String>),
    StreamControlMethodChanged(StreamControlMethod),

    // Templates
    TemplateSelected(DiskTemplate),
    ExecuteTemplate,

    // Function bar actions
    FnView,
    FnCopy,
    FnRename,
    FnMkDir,
    FnNewDisk,
    FnDelete,
    /// Refresh / re-read the active pane's directory listing. Bound to Ctrl+R
    /// and the toolbar "↻ Refresh" button.
    FnRefresh,
    /// Open the selected local file with the OS's default editor (F4 in TC).
    /// Remote files aren't supported in v1 — that needs a download-edit-upload
    /// round-trip which we leave to the user (download via F5, edit locally).
    FnEdit,
    /// Calculate the recursive size of the currently-selected folder on the
    /// active pane. Bound to Space, matching Total Commander. Local-only
    /// for now (remote FTP recursive walk is too slow to be friendly).
    FnSize,
    /// Move the file-list cursor up/down in the active pane (arrow keys).
    /// Wraps around. Scrolls the row into view.
    PaneCursorUp,
    PaneCursorDown,
    /// Activate (Enter) the currently-selected file/folder in the active pane.
    PaneActivate,
    /// Type-to-jump quick search — a printable character was pressed while
    /// the dual-pane browser was active. Routed to the active pane's buffer.
    PaneQuickSearch(char),
    /// Esc — clear the quick-search buffer on the active pane.
    PaneQuickSearchClear,
    ToggleActivePane,
    NavigateUpActivePane,
    SelectAllActivePane,
    SelectNoneActivePane,
    FilterChanged(String),

    // Errors
    ShowError(String),
    ShowInfo(String),
    DismissMessage,

    // Video/Streaming
    Streaming(StreamingMessage),
    ExitFullscreen,

    // Memory Editor
    MemoryEditor(MemoryEditorMessage),
    // Hardware monitor (SID / VIC-II / CIA)
    Monitor(SidMonitorMessage),
    // Machine control
    ResetMachine,
    RebootMachine,
    PauseMachine,
    ResumeMachine,
    PoweroffMachine,
    MenuButton,
    MachineCommandCompleted(Result<String, String>),
    // ── DEVICE tab: drive control / keyboard / debug register ────────
    /// Switch a drive's emulated type. (drive "a"|"b", mode "1541"|"1571"|"1581")
    DeviceSetDriveMode(String, String),
    /// Power a drive on (true) or off (false). (drive, on)
    DeviceDrivePower(String, bool),
    /// Reset a drive. (drive)
    DeviceDriveReset(String),
    /// Text field for keyboard injection.
    DeviceKeyboardInputChanged(String),
    /// Send the buffered text into the running C64 keyboard buffer.
    DeviceSendKeys,
    /// Debug-register write-value text field (hex).
    DeviceDebugRegInputChanged(String),
    /// Read the debug register ($D7FF).
    DeviceReadDebugReg,
    /// Result of a debug-register read.
    DeviceDebugRegRead(Result<u8, String>),
    /// Write the debug register with the buffered value.
    DeviceWriteDebugReg,
    // ── DEVICE tab: debug bus-trace stream capture ───────────────────
    /// Start capturing the debug bus-trace stream.
    DeviceDebugStreamStart,
    /// Stop the debug-stream capture.
    DeviceDebugStreamStop,
    /// Timer tick to refresh capture counters while active.
    DeviceDebugStreamTick,
    /// Save the captured debug stream to a file.
    DeviceDebugStreamSave,
    /// Result of a debug-stream save.
    DeviceDebugStreamSaved(Result<String, String>),
    /// Sink for events we explicitly want to absorb without doing anything
    /// (e.g. clicks on a modal dialog's body so they don't bubble to the
    /// backdrop's dismiss handler).
    Nop,
    /// Eject the disk image from both Drive A and Drive B. Shows a
    /// confirmation dialog first — accidentally clearing a hand-set
    /// mount has no undo, so we make the user confirm explicitly.
    EjectAllDrives,
    /// User confirmed the Eject A+B prompt; actually fire the unmounts.
    EjectAllDrivesConfirmed,
    /// User dismissed the Eject A+B confirmation without confirming.
    EjectCancel,
    EjectCompleted(Result<String, String>),
    /// Re-fire whatever PRG/CRT/SID/disk the local file browser most
    /// recently ran. No-op if no run has happened in this session.
    RunLast,

    // Settings
    DefaultSongDurationChanged(String),
    FontSizeChanged(String),

    // Starting directory settings
    BrowseFileBrowserStartDir,
    FileBrowserStartDirSelected(Option<PathBuf>),
    ClearFileBrowserStartDir,
    BrowseMusicPlayerStartDir,
    MusicPlayerStartDirSelected(Option<PathBuf>),
    ClearMusicPlayerStartDir,
    // Version check
    VersionCheck(VersionCheckMessage),
    OpenReleasePage,
    // Assembly64 Browser
    Assembly64Browser(Assembly64BrowserMessage),
    // BASIC editor
    BasicEditor(BasicEditorMessage),
    // Separate streaming window management
    OpenStreamingWindow,
    StreamingWindowOpened(iced::window::Id),
    WindowClosed(iced::window::Id),
    CloseStreamingWindow,
    // Profile management
    ProfileSelected(String),
    NewProfileNameChanged(String),
    CreateProfile,
    DuplicateProfile,
    DeleteProfile,
    RenameProfile,
    RenameProfileNameChanged(String),
    SaveProfile,
    // Discovery
    StartDiscovery,
    DiscoveryComplete(Vec<discovery::DiscoveredDevice>),
    SelectDiscoveredDevice(discovery::DiscoveredDevice),
    // Drag-and-drop from the OS
    /// Window OS told us a file was dropped on the app — opens a small
    /// dialog asking the user what to do with it (run / mount / open / upload).
    FileDropped(PathBuf),
    /// User picked an action button in the drop dialog.
    DropAction(DropAction),
    /// User dismissed the drop dialog without choosing.
    DropCancel,
    /// Async drop action finished — surface the result message.
    DropCompleted(Result<String, String>),
    /// User clicked the Cancel button while a drop action was in flight.
    /// Aborts the running Task without waiting for the network timeout.
    DropAbort,
    /// Toggle the Help overlay (cheatsheet of every keybind).
    ShowHelp,
    HideHelp,
    /// Single Esc key — context-aware in the update handler: closes the
    /// help overlay first, then exits fullscreen if active, then propagates
    /// to per-pane quick-search clear.
    EscPressed,
    /// Ctrl/Cmd+L — focus the active pane's editable path field so the user
    /// can type a path and press Enter to jump there.
    FocusPathField,
    /// Periodic tick fired while a toast is visible — clears it once the
    /// 4s display window has elapsed.
    ToastTick,
    /// OS asked us to close the given window. We swallow the request and
    /// either close immediately or pop a confirmation modal when a transfer
    /// is in flight.
    WindowCloseRequested(iced::window::Id),
    /// User confirmed they want to close despite an in-flight transfer.
    ConfirmCloseWindow,
    /// User dismissed the close-confirmation modal.
    CancelCloseWindow,
}

/// Action chosen from the drag-and-drop dialog.
#[derive(Debug, Clone)]
pub enum DropAction {
    /// Send the file to the device's run_prg / run_crt / sidplay runner.
    /// `runner` is one of those three literal names.
    RunOnDevice { path: PathBuf, runner: &'static str },
    /// Mount a disk image on the active drive (RO).
    MountDisk { path: PathBuf },
    /// Load text into the BASIC editor and switch to that tab.
    OpenInBasicEditor { path: PathBuf },
    /// FTP-upload the file to the remote browser's current path.
    UploadToRemote { path: PathBuf },
}

impl DropAction {
    /// Status-line text to display while this action is in flight.
    pub fn status_label(&self) -> String {
        match self {
            DropAction::RunOnDevice { runner, .. } => format!("Running via {}", runner),
            DropAction::MountDisk { .. } => "Mounting disk".into(),
            DropAction::OpenInBasicEditor { .. } => "Opening in BASIC editor".into(),
            DropAction::UploadToRemote { .. } => "Uploading to remote".into(),
        }
    }

    /// Set of buttons appropriate for a dropped file with this lowercase
    /// extension. `Upload to remote` and `Cancel` are always present
    /// (they're added by the view); this returns just the type-specific
    /// actions in the order they should appear.
    pub fn available_for(ext: &str, path: &std::path::Path) -> Vec<DropAction> {
        let p = path.to_path_buf();
        match ext {
            "prg" => vec![DropAction::RunOnDevice {
                path: p,
                runner: "run_prg",
            }],
            "crt" => vec![DropAction::RunOnDevice {
                path: p,
                runner: "run_crt",
            }],
            "sid" => vec![DropAction::RunOnDevice {
                path: p,
                runner: "sidplay",
            }],
            "d64" | "d71" | "d81" | "g64" | "g71" => vec![DropAction::MountDisk { path: p }],
            "bas" | "txt" => vec![DropAction::OpenInBasicEditor { path: p }],
            _ => Vec::new(),
        }
    }

    /// Button label for the dialog.
    pub fn button_label(&self) -> &'static str {
        match self {
            DropAction::RunOnDevice { .. } => "▶ Run on device",
            DropAction::MountDisk { .. } => "💾 Mount on Drive A:RO",
            DropAction::OpenInBasicEditor { .. } => "📝 Open in BASIC editor",
            DropAction::UploadToRemote { .. } => "📤 Upload to remote",
        }
    }
}

// Serialize/Deserialize so the active tab can be persisted in settings.json
// and restored on next launch — see `Preferences::last_active_tab`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum Tab {
    DualPaneBrowser,
    MusicPlayer,
    VideoViewer,
    MemoryEditor,
    Monitor,
    Configuration,
    Profiles,
    Assembly64,
    BasicEditor,
    Device,
    Settings,
}

impl std::fmt::Display for Tab {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Tab::DualPaneBrowser => write!(f, "File Browser"),
            Tab::MusicPlayer => write!(f, "Music Player"),
            Tab::VideoViewer => write!(f, "Video Viewer"),
            Tab::MemoryEditor => write!(f, "Memory Editor"),
            Tab::Monitor => write!(f, "HW Monitor"),
            Tab::Configuration => write!(f, "Configuration"),
            Tab::Profiles => write!(f, "Profiles"),
            Tab::Assembly64 => write!(f, "Assembly64"),
            Tab::BasicEditor => write!(f, "BASIC"),
            Tab::Device => write!(f, "Device"),
            Tab::Settings => write!(f, "Settings"),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Pane {
    Left,
    Right,
}

#[derive(Debug, Clone)]
pub struct StatusInfo {
    pub connected: bool,
    pub device_info: Option<String>,
    pub mounted_disks: Vec<(String, String)>,
}

#[derive(Debug, Clone)]
pub enum UserMessage {
    Error(String),
    Info(String),
}

pub struct Ultimate64Browser {
    active_tab: Tab,

    // Dual-pane file browsers
    left_browser: FileBrowser,
    remote_browser: RemoteBrowser,
    active_pane: Pane,

    music_player: MusicPlayer,
    memory_editor: MemoryEditor,
    sid_monitor: SidMonitor,
    config_editor: ConfigEditor,
    settings: AppSettings,
    template_manager: TemplateManager,
    selected_template: Option<DiskTemplate>,
    connection: Option<Arc<Mutex<Rest>>>,
    host_url: Option<String>, // Store host URL for direct HTTP requests
    status: StatusInfo,
    /// DEVICE tab: buffered text to inject into the running C64 keyboard.
    device_keyboard_input: String,
    /// DEVICE tab: input for the debug-register write value (hex).
    device_debugreg_input: String,
    /// DEVICE tab: last value read from the debug register ($D7FF).
    device_debugreg_value: Option<u8>,
    /// DEVICE tab: raw debug bus-trace stream capture engine.
    debug_stream: debug_stream::DebugStreamCapture,
    /// Count of back-to-back failed status polls. We only flip the UI to
    /// "Not connected" once it crosses [`MAX_TRANSIENT_STATUS_FAILURES`],
    /// so a 2-3 second reboot blip doesn't show as a disconnect.
    consecutive_status_failures: u8,

    // User messages (errors and info)
    user_message: Option<UserMessage>,

    // Input fields
    host_input: String,
    password_input: String,
    font_size_input: String,

    // Video streaming
    video_streaming: VideoStreaming,
    // Update notification
    new_version: Option<NewVersionInfo>,
    assembly64_browser: Assembly64Browser,
    basic_editor: BasicEditor,
    /// File the OS dropped on our window — when `Some`, the view renders
    /// a centered modal asking the user what to do with it.
    pending_drop: Option<PathBuf>,
    /// True while a drop action is sending data to the device — disables
    /// the dialog buttons so a slow upload can't be triggered twice.
    drop_in_flight: bool,
    /// Handle for the in-flight drop task so the Cancel button can abort.
    drop_handle: Option<iced::task::Handle>,
    /// True while the Eject A+B confirmation modal is showing. The actual
    /// eject only fires after the user explicitly confirms — accidentally
    /// clearing a mount has no undo.
    pending_eject_confirm: bool,
    /// When true, the Help overlay (cheatsheet of every keybind) is rendered
    /// on top of whatever tab is active. Toggled by `?`.
    show_help: bool,
    // Separate streaming window
    streaming_window_id: Option<window::Id>,
    main_window_id: Option<window::Id>,

    // Profile management (app settings profiles)
    profile_manager: ProfileManager,
    // Device profile manager (Ultimate64 config profiles)
    device_profile_manager: DeviceProfileManager,
    new_profile_name: String,
    rename_profile_name: String,
    // Device discovery
    is_discovering: bool,
    discovered_devices: Vec<DiscoveredDevice>,

    /// Shared progress state for copy operations between panes
    copy_progress: Arc<std::sync::Mutex<Option<crate::ftp_ops::TransferProgress>>>,

    /// If set, a local→remote copy is waiting for the user to confirm
    /// overwriting existing files on the device.
    pending_copy: Option<PendingCopy>,

    /// Transient success banner for completed long ops (FTP copy, drag-drop
    /// upload, eject A+B, etc). Auto-fades after [`TOAST_DURATION_SECS`].
    toast: Option<(String, std::time::Instant)>,

    /// Set when the user tried to close the window while a transfer was in
    /// flight. Shows a confirm dialog; clearing it aborts the close, the
    /// `ConfirmClose` button on the dialog finishes it.
    pending_close: Option<window::Id>,
}

/// How long a toast stays on screen before [`Message::ToastTick`] clears it.
const TOAST_DURATION_SECS: u64 = 4;

/// Staged local→remote copy, held while we ask the user whether to overwrite.
#[derive(Debug, Clone)]
struct PendingCopy {
    items: Vec<std::path::PathBuf>,
    remote_dest: String,
    conflicts: Vec<String>,
}

impl Ultimate64Browser {
    fn new() -> (Self, Task<Message>) {
        log::info!("Initializing application...");

        let profile_manager = ProfileManager::load();
        let settings = profile_manager.active_settings().clone();
        log::info!("Active profile: {}", profile_manager.active_profile);

        // One-shot rename of the legacy `CSDB/` downloads folder to
        // `Assembly64/` so users upgrading from the old browser keep their
        // prior downloads under the new toolbar shortcut.
        if let Some(cfg) = dirs::config_dir() {
            file_browser::migrate_csdb_to_assembly(&cfg.join("ultimate64-manager"));
        }

        // Create music player with configured starting directory
        let mut music_player =
            MusicPlayer::new(settings.default_paths.music_player_start_dir.clone());
        music_player.set_default_song_duration(settings.preferences.default_song_duration);

        // Create file browser with configured starting directory
        let left_browser = FileBrowser::new(settings.default_paths.file_browser_start_dir.clone());

        // Load window icon
        let icon = load_window_icon();

        // Open main window
        let (main_window_id, open_main_window) = iced::window::open(iced::window::Settings {
            size: iced::Size::new(1200.0, 800.0),
            min_size: Some(iced::Size::new(800.0, 600.0)),
            icon: icon,
            // Intercept close so we can warn when a transfer is in flight.
            // See [`Message::WindowCloseRequested`].
            exit_on_close_request: false,
            ..Default::default()
        });

        let mut app = Self {
            // Restore the tab the user closed in last session, or fall back
            // to the file browser for first-time launches.
            active_tab: settings
                .preferences
                .last_active_tab
                .unwrap_or(Tab::DualPaneBrowser),
            left_browser,
            remote_browser: RemoteBrowser::new(),
            active_pane: Pane::Left,
            music_player,
            memory_editor: MemoryEditor::new(),
            sid_monitor: SidMonitor::new(),
            config_editor: ConfigEditor::new(),
            host_input: settings.connection.host.clone(),
            password_input: settings.connection.password.clone().unwrap_or_default(),
            font_size_input: settings.preferences.font_size.to_string(),
            settings: settings.clone(),
            host_url: None,
            device_keyboard_input: String::new(),
            device_debugreg_input: String::new(),
            device_debugreg_value: None,
            debug_stream: debug_stream::DebugStreamCapture::new(),
            template_manager: TemplateManager::new(),
            selected_template: None,
            connection: None,
            new_version: None,
            status: StatusInfo {
                connected: false,
                device_info: None,
                mounted_disks: Vec::new(),
            },
            consecutive_status_failures: 0,
            user_message: None,
            video_streaming: VideoStreaming::new(),
            assembly64_browser: Assembly64Browser::new(),
            basic_editor: BasicEditor::new(),
            pending_drop: None,
            drop_in_flight: false,
            drop_handle: None,
            pending_eject_confirm: false,
            show_help: false,
            main_window_id: Some(main_window_id),
            streaming_window_id: None,
            profile_manager,
            device_profile_manager: DeviceProfileManager::new(),
            new_profile_name: String::new(),
            rename_profile_name: String::new(),
            is_discovering: false,
            discovered_devices: Vec::new(),
            copy_progress: Arc::new(std::sync::Mutex::new(None)),
            pending_copy: None,
            toast: None,
            pending_close: None,
        };
        app.video_streaming
            .set_stream_control_method(settings.connection.stream_control_method);

        // Check for updates on startup
        let version_check_cmd =
            version_check::check_for_updates(APP_VERSION).map(Message::VersionCheck);

        // Build init tasks
        let mut init_tasks = vec![
            open_main_window.map(|_| Message::RefreshStatus),
            version_check_cmd,
        ];

        // Auto-connect if host is configured
        if !settings.connection.host.is_empty() {
            log::info!(
                "Auto-connecting to configured host: {}",
                settings.connection.host
            );
            app.establish_connection();
            init_tasks.push(Task::perform(
                async {
                    tokio::time::sleep(tokio::time::Duration::from_millis(1000)).await;
                },
                |_| Message::RefreshStatus,
            ));
        } else {
            log::info!("No host configured, waiting for user input");
        }

        (app, Task::batch(init_tasks))
    }

    fn title(&self, window_id: iced::window::Id) -> String {
        if Some(window_id) == self.streaming_window_id {
            return "Ultimate64 Video Stream".to_string();
        }
        if Some(window_id) == self.main_window_id {
            // Format: "Ultimate64 Manager — <host> · <tab>"  when connected,
            //         "Ultimate64 Manager · Disconnected · <tab>"  otherwise.
            // The tab name on the tail means people running multiple instances
            // (one per device) can tell at a glance which window is which.
            let location = if self.status.connected {
                self.settings.connection.host.as_str()
            } else {
                "Disconnected"
            };
            return format!("Ultimate64 Manager — {} · {}", location, self.active_tab);
        }
        // Fallback
        "Ultimate64 Manager".to_string()
    }
    fn theme(&self, _window_id: iced::window::Id) -> Theme {
        // Custom dark theme with lighter blue (like the reference screenshot)
        Theme::custom(
            "Ultimate64 Dark".to_string(),
            iced::theme::Palette {
                background: iced::Color::from_rgb(0.15, 0.15, 0.18), // Dark background
                text: iced::Color::from_rgb(0.9, 0.9, 0.9),          // Light text
                primary: iced::Color::from_rgb(0.45, 0.52, 0.85),    // Lighter blue
                success: iced::Color::from_rgb(0.3, 0.7, 0.3),       // Green
                danger: iced::Color::from_rgb(0.8, 0.3, 0.3),        // Red
                warning: iced::Color::from_rgb(0.9, 0.7, 0.2),       // Yellow/Orange
            },
        )
    }

    /// Gather the device context handed to every tab via `TabController::update`.
    fn tab_context(&self) -> TabContext {
        TabContext {
            connection: self.connection.clone(),
            host: Some(self.settings.connection.host.clone()),
            host_url: self.host_url.clone(),
            password: self.settings.connection.password.clone(),
        }
    }

    fn update(&mut self, message: Message) -> Task<Message> {
        // Uniform device context handed to every tab's `TabController::update`.
        // Built once (owned) so it can be cloned into tab calls without holding
        // a borrow on `self` across the mutable per-tab borrows below.
        let ctx = self.tab_context();
        match message {
            Message::SaveProfile => self.handle_save_profile(),
            Message::StartDiscovery => self.handle_start_discovery(),

            Message::DiscoveryComplete(devices) => self.handle_discovery_complete(devices),

            Message::SelectDiscoveredDevice(device) => self.handle_select_discovered_device(device),
            Message::TabSelected(tab) => {
                log::debug!("Tab selected: {:?}", tab);
                self.active_tab = tab;
                // Remember the choice so the next launch opens on the same
                // tab. A failed disk write is logged but never blocks the
                // UI — losing a single tab preference is not worth a popup.
                self.settings.preferences.last_active_tab = Some(tab);
                if let Err(e) = self.settings.save() {
                    log::warn!("Could not save settings after tab switch: {}", e);
                }
                // Auto-load latest entries when Assembly64 tab is first opened.
                // Also kick off a background refresh of the dropdown
                // presets and category map (one-shot per session).
                if tab == Tab::Assembly64 && !self.assembly64_browser.has_content() {
                    let refresh = self
                        .assembly64_browser
                        .update(Assembly64BrowserMessage::RefreshPresets, ctx.clone())
                        .map(Message::Assembly64Browser);
                    let search = self
                        .assembly64_browser
                        .update(Assembly64BrowserMessage::SearchSubmit, ctx.clone())
                        .map(Message::Assembly64Browser);
                    return Task::batch([refresh, search]);
                }
                Task::none()
            }
            Message::CloseStreamingWindow => self.handle_close_streaming_window(),
            Message::MemoryEditor(msg) => self
                .memory_editor
                .update(msg, ctx.clone())
                .map(Message::MemoryEditor),
            Message::Monitor(msg) => self
                .sid_monitor
                .update(msg, ctx.clone())
                .map(Message::Monitor),
            Message::LeftBrowser(msg) => {
                // User interaction with left pane makes it active
                // (exclude async completion callbacks which aren't user-initiated)
                if !matches!(
                    &msg,
                    FileBrowserMessage::MountCompleted(_)
                        | FileBrowserMessage::RunDiskCompleted(_)
                        | FileBrowserMessage::LoadCompleted(_)
                        | FileBrowserMessage::ZipExtracted(_)
                        | FileBrowserMessage::DiskInfoLoaded(_)
                        | FileBrowserMessage::ContentPreviewLoaded(_)
                        | FileBrowserMessage::DriveCheckComplete(_, _)
                        | FileBrowserMessage::EnableDriveComplete(_)
                        | FileBrowserMessage::RestoreScrollOffset(_)
                        | FileBrowserMessage::DeleteComplete(_)
                ) {
                    self.active_pane = Pane::Left;
                }

                // Check if this is a "run" operation that should stop music
                let should_stop_music = matches!(
                    &msg,
                    FileBrowserMessage::RunDisk(_, _) | FileBrowserMessage::LoadAndRun(_)
                ) && self.music_player.playback_state
                    == PlaybackState::Playing;

                let should_refresh = matches!(msg, FileBrowserMessage::MountCompleted(Ok(_)));

                let cmd = self
                    .left_browser
                    .update(msg, ctx.clone())
                    .map(Message::LeftBrowser);

                let mut commands = vec![cmd];

                // Stop music player if running a cartridge/disk
                if should_stop_music {
                    log::info!("Stopping music player - running cartridge/disk");
                    commands.push(
                        self.music_player
                            .update(MusicPlayerMessage::Stop, ctx.clone())
                            .map(Message::MusicPlayer),
                    );
                }

                if should_refresh {
                    commands.push(Task::perform(
                        async {
                            tokio::time::sleep(tokio::time::Duration::from_millis(500)).await;
                        },
                        |_| Message::RefreshStatus,
                    ));
                }

                Task::batch(commands)
            }

            Message::RemoteBrowser(msg) => {
                // User interaction with right pane makes it active
                // (exclude async completion callbacks which aren't user-initiated)
                if !matches!(
                    &msg,
                    RemoteBrowserMessage::FilesLoaded(_)
                        | RemoteBrowserMessage::DownloadComplete(_)
                        | RemoteBrowserMessage::UploadComplete(_)
                        | RemoteBrowserMessage::UploadDirectoryComplete(_)
                        | RemoteBrowserMessage::RunnerComplete(_)
                        | RemoteBrowserMessage::MountComplete(_)
                        | RemoteBrowserMessage::DownloadBatchComplete(_)
                        | RemoteBrowserMessage::DiskInfoLoaded(_)
                        | RemoteBrowserMessage::ContentPreviewLoaded(_)
                        | RemoteBrowserMessage::CreateDiskComplete(_)
                        | RemoteBrowserMessage::CreateDirComplete(_)
                        | RemoteBrowserMessage::DeleteComplete(_)
                        | RemoteBrowserMessage::RenameComplete(_)
                        | RemoteBrowserMessage::ProgressTick
                ) {
                    self.active_pane = Pane::Right;
                }

                // Check if this is a "run" operation that should stop music
                let should_stop_music = matches!(
                    &msg,
                    RemoteBrowserMessage::RunPrg(_)
                        | RemoteBrowserMessage::RunCrt(_)
                        | RemoteBrowserMessage::PlaySid(_)
                        | RemoteBrowserMessage::PlayMod(_)
                        | RemoteBrowserMessage::RunDisk(_, _)
                ) && self.music_player.playback_state
                    == PlaybackState::Playing;

                // Intercept DownloadBatchComplete to show result and refresh local browser
                if let RemoteBrowserMessage::DownloadBatchComplete(ref result) = msg {
                    match result {
                        Ok(m) => {
                            self.user_message = Some(UserMessage::Info(m.clone()));
                        }
                        Err(e) => {
                            self.user_message = Some(UserMessage::Error(e.clone()));
                        }
                    }
                    let cmd = self
                        .remote_browser
                        .update(msg, ctx.clone())
                        .map(Message::RemoteBrowser);
                    let refresh = self
                        .left_browser
                        .update(FileBrowserMessage::RefreshFiles, ctx.clone())
                        .map(Message::LeftBrowser);
                    return Task::batch(vec![cmd, refresh]);
                }

                // Intercept UploadDirectoryComplete to show result in main status bar
                if let RemoteBrowserMessage::UploadDirectoryComplete(ref result) = msg {
                    match result {
                        Ok(m) => {
                            self.user_message = Some(UserMessage::Info(m.clone()));
                        }
                        Err(e) => {
                            self.user_message = Some(UserMessage::Error(e.clone()));
                        }
                    }
                }

                let cmd = self
                    .remote_browser
                    .update(msg, ctx.clone())
                    .map(Message::RemoteBrowser);

                if should_stop_music {
                    log::info!("Stopping music player - running file from Ultimate64");
                    Task::batch(vec![
                        cmd,
                        self.music_player
                            .update(MusicPlayerMessage::Stop, ctx.clone())
                            .map(Message::MusicPlayer),
                    ])
                } else {
                    cmd
                }
            }
            Message::ActivePaneChanged(pane) => {
                self.active_pane = pane;
                Task::none()
            }

            Message::FocusPathField => {
                // Ctrl+L focuses whichever pane is active. The matching
                // `text_input` is registered with the same `Id` in each
                // browser's `build_nav_row`.
                let id = match self.active_pane {
                    Pane::Left => iced::widget::Id::new(file_browser::PATH_INPUT_ID),
                    Pane::Right => iced::widget::Id::new(remote_browser::PATH_INPUT_ID),
                };
                iced::widget::operation::focus(id)
            }
            Message::PaneCursorUp => {
                self.dispatch_local_pane_message(FileBrowserMessage::MoveSelectionUp)
            }
            Message::PaneCursorDown => {
                self.dispatch_local_pane_message(FileBrowserMessage::MoveSelectionDown)
            }
            Message::PaneActivate => {
                self.dispatch_local_pane_message(FileBrowserMessage::ActivateSelection)
            }
            Message::PaneQuickSearch(ch) => {
                self.dispatch_local_pane_message(FileBrowserMessage::QuickSearchInput(ch))
            }
            Message::PaneQuickSearchClear => {
                self.dispatch_local_pane_message(FileBrowserMessage::QuickSearchClear)
            }

            Message::FnSize => match self.active_pane {
                Pane::Left => self
                    .left_browser
                    .update(FileBrowserMessage::CalculateSelectedSize, ctx.clone())
                    .map(Message::LeftBrowser),
                Pane::Right => {
                    self.user_message = Some(UserMessage::Info(
                        "Folder size on Space is local-only — FTP walk is too slow".to_string(),
                    ));
                    Task::none()
                }
            },

            Message::FnEdit => {
                // F4 Edit — open the selected file in the OS default editor.
                // Only meaningful on the local pane; remote files would need
                // a download-edit-upload round-trip that's better done
                // explicitly by the user.
                match self.active_pane {
                    Pane::Left => {
                        if let Some(path) = self.left_browser.get_selected_file().cloned() {
                            match open::that_detached(&path) {
                                Ok(()) => {
                                    self.user_message = Some(UserMessage::Info(format!(
                                        "Opened in editor: {}",
                                        path.display()
                                    )));
                                }
                                Err(e) => {
                                    self.user_message =
                                        Some(UserMessage::Error(format!("Open failed: {}", e)));
                                }
                            }
                        } else {
                            self.user_message = Some(UserMessage::Info(
                                "Click a file first, then press F4 to edit".to_string(),
                            ));
                        }
                    }
                    Pane::Right => {
                        self.user_message = Some(UserMessage::Info(
                            "F4 Edit works on local files only — download first with F5"
                                .to_string(),
                        ));
                    }
                }
                Task::none()
            }

            Message::FnView => {
                // Preview selected file in active pane
                match self.active_pane {
                    Pane::Left => {
                        if let Some(path) = self.left_browser.get_selected_file().cloned() {
                            return self
                                .left_browser
                                .update(FileBrowserMessage::ShowContentPreview(path), ctx.clone())
                                .map(Message::LeftBrowser);
                        }
                        Task::none()
                    }
                    Pane::Right => {
                        if let Some(path) =
                            self.remote_browser.get_selected_file().map(String::from)
                        {
                            return self
                                .remote_browser
                                .update(RemoteBrowserMessage::ShowContentPreview(path), ctx.clone())
                                .map(Message::RemoteBrowser);
                        }
                        Task::none()
                    }
                }
            }

            Message::FnCopy => {
                // Copy from active pane to other pane
                match self.active_pane {
                    Pane::Left => self.update(Message::CopyLocalToRemote),
                    Pane::Right => self.update(Message::CopyRemoteToLocal),
                }
            }

            Message::FnRefresh => match self.active_pane {
                Pane::Left => self
                    .left_browser
                    .update(FileBrowserMessage::RefreshFiles, ctx.clone())
                    .map(Message::LeftBrowser),
                Pane::Right => self
                    .remote_browser
                    .update(RemoteBrowserMessage::RefreshFiles, ctx.clone())
                    .map(Message::RemoteBrowser),
            },

            Message::FnMkDir => match self.active_pane {
                Pane::Left => self
                    .left_browser
                    .update(FileBrowserMessage::ShowCreateDir, ctx.clone())
                    .map(Message::LeftBrowser),
                Pane::Right => self
                    .remote_browser
                    .update(RemoteBrowserMessage::ShowCreateDir, ctx.clone())
                    .map(Message::RemoteBrowser),
            },

            Message::FnNewDisk => match self.active_pane {
                Pane::Left => self
                    .left_browser
                    .update(FileBrowserMessage::ShowCreateDisk, ctx.clone())
                    .map(Message::LeftBrowser),
                Pane::Right => self
                    .remote_browser
                    .update(RemoteBrowserMessage::ShowCreateDisk, ctx.clone())
                    .map(Message::RemoteBrowser),
            },

            Message::FnDelete => {
                // Delete selected files in active pane
                match self.active_pane {
                    Pane::Left => {
                        let checked_count = self.left_browser.get_checked_files().len();
                        if checked_count > 0 {
                            return self
                                .left_browser
                                .update(FileBrowserMessage::DeleteChecked, ctx.clone())
                                .map(Message::LeftBrowser);
                        }
                        Task::none()
                    }
                    Pane::Right => {
                        let checked_count = self.remote_browser.checked_files.len();
                        if checked_count > 0 {
                            return self
                                .remote_browser
                                .update(RemoteBrowserMessage::DeleteChecked, ctx.clone())
                                .map(Message::RemoteBrowser);
                        }
                        Task::none()
                    }
                }
            }

            Message::FilterChanged(text) => match self.active_pane {
                Pane::Left => self
                    .left_browser
                    .update(FileBrowserMessage::FilterChanged(text), ctx.clone())
                    .map(Message::LeftBrowser),
                Pane::Right => self
                    .remote_browser
                    .update(RemoteBrowserMessage::FilterChanged(text), ctx.clone())
                    .map(Message::RemoteBrowser),
            },

            Message::ToggleActivePane => {
                self.active_pane = match self.active_pane {
                    Pane::Left => Pane::Right,
                    Pane::Right => Pane::Left,
                };
                Task::none()
            }

            Message::NavigateUpActivePane => match self.active_pane {
                Pane::Left => self
                    .left_browser
                    .update(FileBrowserMessage::NavigateUp, ctx.clone())
                    .map(Message::LeftBrowser),
                Pane::Right => self
                    .remote_browser
                    .update(RemoteBrowserMessage::NavigateUp, ctx.clone())
                    .map(Message::RemoteBrowser),
            },

            Message::SelectAllActivePane => match self.active_pane {
                Pane::Left => self
                    .left_browser
                    .update(FileBrowserMessage::SelectAll, ctx.clone())
                    .map(Message::LeftBrowser),
                Pane::Right => self
                    .remote_browser
                    .update(RemoteBrowserMessage::SelectAll, ctx.clone())
                    .map(Message::RemoteBrowser),
            },

            Message::SelectNoneActivePane => match self.active_pane {
                Pane::Left => self
                    .left_browser
                    .update(FileBrowserMessage::SelectNone, ctx.clone())
                    .map(Message::LeftBrowser),
                Pane::Right => self
                    .remote_browser
                    .update(RemoteBrowserMessage::SelectNone, ctx.clone())
                    .map(Message::RemoteBrowser),
            },

            Message::FnRename => {
                match self.active_pane {
                    Pane::Left => {
                        if let Some(path) = self.left_browser.get_selected_file().cloned() {
                            return self
                                .left_browser
                                .update(FileBrowserMessage::RenameFile(path), ctx.clone())
                                .map(Message::LeftBrowser);
                        }
                        self.user_message = Some(UserMessage::Info(
                            "Click a file first, then press F2 to rename".to_string(),
                        ));
                        Task::none()
                    }
                    Pane::Right => {
                        // Rename the selected file on remote
                        if let Some(ref selected) = self.remote_browser.selected_file {
                            let path = selected.clone();
                            return self
                                .remote_browser
                                .update(RemoteBrowserMessage::RenameFile(path), ctx.clone())
                                .map(Message::RemoteBrowser);
                        }
                        self.user_message = Some(UserMessage::Info(
                            "Click a file first, then press F2 to rename".to_string(),
                        ));
                        Task::none()
                    }
                }
            }

            Message::CopyLocalToRemote => self.handle_copy_local_to_remote(),
            Message::CopyOverwriteCancel => self.handle_copy_overwrite_cancel(),
            Message::CopyOverwriteConfirm => self.handle_copy_overwrite_confirm(),
            Message::CopyRemoteToLocal => self.handle_copy_remote_to_local(),

            Message::CopyCancel => self.handle_copy_cancel(),
            Message::CopyProgressTick => self.handle_copy_progress_tick(),
            Message::CopyComplete(result) => self.handle_copy_complete(result, ctx.clone()),
            Message::VersionCheck(msg) => {
                match msg {
                    VersionCheckMessage::CheckComplete(result) => match result {
                        Ok(Some(info)) => {
                            log::info!("New version available: {}", info.version);
                            self.new_version = Some(info);
                        }
                        Ok(None) => {
                            log::debug!("Running latest version");
                        }
                        Err(e) => {
                            log::warn!("Version check failed: {}", e);
                        }
                    },
                }
                Task::none()
            }

            Message::OpenReleasePage => {
                if let Some(info) = &self.new_version {
                    let _ = open::that(&info.download_url);
                }
                Task::none()
            }
            Message::MusicPlayer(msg) => {
                // Check if we need to pause or resume the machine
                let was_paused = self.music_player.playback_state == PlaybackState::Paused;

                // Intercept Pause - also pause the machine
                if let MusicPlayerMessage::Pause = &msg {
                    let cmd = self
                        .music_player
                        .update(msg, ctx.clone())
                        .map(Message::MusicPlayer);

                    // Also send PauseMachine command
                    if let Some(host) = &self.host_url {
                        let url = format!("{}/v1/machine:pause", host);
                        let pause_cmd = Task::perform(
                            async move {
                                let client =
                                    crate::net_utils::build_device_client(REST_TIMEOUT_SECS)?;
                                client
                                    .put(&url)
                                    .send()
                                    .await
                                    .map_err(|e| format!("Pause failed: {}", e))?;
                                Ok("Machine paused".to_string())
                            },
                            Message::MachineCommandCompleted,
                        );
                        return Task::batch([cmd, pause_cmd]);
                    }
                    return cmd;
                }

                // Intercept Play when resuming from pause - also resume the machine
                if let MusicPlayerMessage::Play = &msg {
                    if was_paused {
                        let cmd = self
                            .music_player
                            .update(msg, ctx.clone())
                            .map(Message::MusicPlayer);

                        // Also send ResumeMachine command
                        if let Some(host) = &self.host_url {
                            let url = format!("{}/v1/machine:resume", host);
                            let resume_cmd = Task::perform(
                                async move {
                                    let client =
                                        crate::net_utils::build_device_client(REST_TIMEOUT_SECS)?;
                                    client
                                        .put(&url)
                                        .send()
                                        .await
                                        .map_err(|e| format!("Resume failed: {}", e))?;
                                    Ok("Machine resumed".to_string())
                                },
                                Message::MachineCommandCompleted,
                            );
                            return Task::batch([cmd, resume_cmd]);
                        }
                        return cmd;
                    }
                }

                self.music_player
                    .update(msg, ctx.clone())
                    .map(Message::MusicPlayer)
            }

            Message::ConfigEditor(msg) => self
                .config_editor
                .update(msg, ctx.clone())
                .map(Message::ConfigEditor),

            Message::DeviceProfileManager(msg) => {
                // Provide streaming frame buffer for screenshot capture
                self.device_profile_manager
                    .set_streaming_frame(self.video_streaming.frame_buffer.clone());
                self.device_profile_manager
                    .update(msg, ctx.clone())
                    .map(Message::DeviceProfileManager)
            }

            Message::HostInputChanged(value) => self.handle_host_input_changed(value),

            Message::PasswordInputChanged(value) => self.handle_password_input_changed(value),

            Message::ConnectPressed => self.handle_connect_pressed(),

            Message::DisconnectPressed => self.handle_disconnect_pressed(ctx.clone()),
            Message::StreamControlMethodChanged(method) => {
                self.handle_stream_control_method_changed(method)
            }
            Message::RefreshStatus => self.handle_refresh_status(),
            Message::ProfileSelected(name) => self.handle_profile_selected(name),

            Message::NewProfileNameChanged(name) => self.handle_new_profile_name_changed(name),

            Message::CreateProfile => self.handle_create_profile(),

            Message::DuplicateProfile => self.handle_duplicate_profile(),

            Message::DeleteProfile => self.handle_delete_profile(),

            Message::RenameProfileNameChanged(name) => {
                self.handle_rename_profile_name_changed(name)
            }

            Message::RenameProfile => self.handle_rename_profile(),
            Message::Assembly64Browser(msg) => self
                .assembly64_browser
                .update(msg, ctx.clone())
                .map(Message::Assembly64Browser),
            Message::BasicEditor(msg) => self
                .basic_editor
                .update(msg, ctx.clone())
                .map(Message::BasicEditor),
            Message::RefreshAfterConnect => self.handle_refresh_after_connect(ctx.clone()),

            Message::StatusUpdated(result) => self.handle_status_updated(result, ctx.clone()),

            Message::TemplateSelected(template) => self.handle_template_selected(template),

            Message::ExecuteTemplate => self.handle_execute_template(),

            Message::ShowError(error) => {
                log::error!("Error: {}", error);
                self.user_message = Some(UserMessage::Error(error));
                Task::none()
            }

            Message::ShowInfo(info) => {
                log::info!("Info: {}", info);
                self.user_message = Some(UserMessage::Info(info));
                Task::none()
            }

            Message::DismissMessage => self.handle_dismiss_message(),

            Message::Streaming(msg) => {
                self.video_streaming
                    .set_stream_control_method(self.settings.connection.stream_control_method);

                self.video_streaming
                    .set_api_password(self.settings.connection.password.clone());
                // Handle screenshot result for user message
                if let StreamingMessage::OpenInSeparateWindow = msg {
                    return Task::perform(async {}, |_| Message::OpenStreamingWindow);
                }
                if let StreamingMessage::ScreenshotComplete(ref result) = msg {
                    match result {
                        Ok(path) => {
                            self.user_message =
                                Some(UserMessage::Info(format!("Screenshot saved: {}", path)));
                        }
                        Err(e) => {
                            self.user_message =
                                Some(UserMessage::Error(format!("Screenshot failed: {}", e)));
                        }
                    }
                }

                // Handle fullscreen toggle - change window mode for true fullscreen
                if let StreamingMessage::ToggleFullscreen = msg {
                    self.video_streaming.is_fullscreen = !self.video_streaming.is_fullscreen;

                    let mode = if self.video_streaming.is_fullscreen {
                        iced::window::Mode::Fullscreen
                    } else {
                        iced::window::Mode::Windowed
                    };

                    // Determine which window to make fullscreen
                    if let Some(streaming_id) = self.streaming_window_id {
                        return window::set_mode(streaming_id, mode)
                            .map(|_: ()| Message::RefreshStatus);
                    } else {
                        return iced::window::oldest()
                            .and_then(move |id| iced::window::set_mode(id, mode))
                            .map(|_: ()| Message::RefreshStatus);
                    }
                }

                // Handle open in new window request
                if let StreamingMessage::OpenInSeparateWindow = msg {
                    return Task::done(Message::OpenStreamingWindow);
                }

                // Handle keyboard command - intercept before passing to streaming
                if let StreamingMessage::SendCommand = msg {
                    let command = self.video_streaming.command_input.clone();
                    if !command.is_empty() {
                        if let Some(conn) = &self.connection {
                            let conn = conn.clone();
                            let cmd = command.clone();
                            // Add to history with prompt
                            self.video_streaming
                                .command_history
                                .push(format!("> {}", command));
                            self.video_streaming.command_input.clear();

                            return Task::perform(
                                async move {
                                    let result = tokio::time::timeout(
                                        tokio::time::Duration::from_secs(REST_TIMEOUT_SECS),
                                        tokio::task::spawn_blocking(move || {
                                            let conn = conn.lock().unwrap();
                                            // Add newline to execute the command
                                            conn.type_text(&format!("{}\n", cmd))
                                                .map(|_| format!("Sent: {}", cmd))
                                                .map_err(|e| format!("Failed to send: {}", e))
                                        }),
                                    )
                                    .await;

                                    match result {
                                        Ok(Ok(r)) => r,
                                        Ok(Err(e)) => Err(format!("Task error: {}", e)),
                                        Err(_) => {
                                            Err("Command timed out. The device may be offline or Web Remote Control is disabled."
                                                .to_string())
                                        }
                                    }
                                },
                                |result| match result {
                                    Ok(msg) => {
                                        Message::Streaming(StreamingMessage::CommandSent(Ok(msg)))
                                    }
                                    Err(e) => {
                                        Message::Streaming(StreamingMessage::CommandSent(Err(e)))
                                    }
                                },
                            );
                        } else {
                            self.video_streaming
                                .command_history
                                .push("> ".to_string() + &command);
                            self.video_streaming
                                .command_history
                                .push("Error: Not connected to Ultimate64".to_string());
                            self.video_streaming.command_input.clear();
                        }
                    }
                    return Task::none();
                }

                self.video_streaming
                    .update(msg, ctx.clone())
                    .map(Message::Streaming)
            }

            Message::EscPressed => self.handle_esc_pressed(),

            Message::ExitFullscreen => self.handle_exit_fullscreen(),

            Message::OpenStreamingWindow => self.handle_open_streaming_window(),

            Message::StreamingWindowOpened(id) => self.handle_streaming_window_opened(id),

            Message::WindowCloseRequested(id) => self.handle_window_close_requested(id),
            Message::ConfirmCloseWindow => self.handle_confirm_close_window(),
            Message::CancelCloseWindow => self.handle_cancel_close_window(),
            Message::WindowClosed(id) => self.handle_window_closed(id),
            // ── Drag-and-drop from the OS ───────────────────────────────
            Message::FileDropped(path) => self.handle_file_dropped(path),
            Message::DropCancel => self.handle_drop_cancel(),
            Message::DropAction(action) => self.handle_drop_action(action),
            Message::DropCompleted(result) => self.handle_drop_completed(result),
            Message::DropAbort => self.handle_drop_abort(),
            Message::ShowHelp => self.handle_show_help(),
            Message::HideHelp => self.handle_hide_help(),

            Message::Nop => Task::none(),

            Message::ToastTick => self.handle_toast_tick(),

            Message::EjectAllDrives => self.handle_eject_all_drives(),
            Message::EjectCancel => self.handle_eject_cancel(),
            Message::EjectAllDrivesConfirmed => self.handle_eject_all_drives_confirmed(),
            Message::EjectCompleted(result) => self.handle_eject_completed(result),
            Message::RunLast => {
                // Re-fire the most recent successful run/mount via the
                // file_browser's own message bus. Cloning the LastRun is
                // cheap (PathBuf + small String).
                let Some(last) = self.left_browser.last_run().cloned() else {
                    self.user_message = Some(UserMessage::Info("Nothing to re-run yet".into()));
                    return Task::none();
                };
                if !last.path().exists() {
                    self.user_message = Some(UserMessage::Error(format!(
                        "Run-last target no longer exists: {}",
                        last.path().display()
                    )));
                    self.left_browser.clear_last_run();
                    return Task::none();
                }
                let msg = match last {
                    file_browser::LastRun::Prg(p) | file_browser::LastRun::Crt(p) => {
                        FileBrowserMessage::LoadAndRun(p)
                    }
                    file_browser::LastRun::Sid(p) => FileBrowserMessage::PlaySid(p),
                    file_browser::LastRun::Disk { path, drive } => {
                        FileBrowserMessage::RunDisk(path, drive)
                    }
                };
                self.left_browser
                    .update(msg, ctx.clone())
                    .map(Message::LeftBrowser)
            }

            Message::ResetMachine => self.handle_reset_machine(),
            Message::RebootMachine => self.handle_reboot_machine(),
            Message::PauseMachine => self.handle_pause_machine(),
            Message::ResumeMachine => self.handle_resume_machine(),
            Message::PoweroffMachine => self.handle_poweroff_machine(),
            Message::MenuButton => self.handle_menu_button(),
            Message::MachineCommandCompleted(result) => {
                self.handle_machine_command_completed(result)
            }

            // ── DEVICE tab handlers ──────────────────────────────────────
            Message::DeviceSetDriveMode(drive, mode) => {
                self.handle_device_set_drive_mode(drive, mode)
            }
            Message::DeviceDrivePower(drive, on) => self.handle_device_drive_power(drive, on),
            Message::DeviceDriveReset(drive) => self.handle_device_drive_reset(drive),
            Message::DeviceKeyboardInputChanged(value) => {
                self.device_keyboard_input = value;
                Task::none()
            }
            Message::DeviceSendKeys => self.handle_device_send_keys(),
            Message::DeviceDebugRegInputChanged(value) => {
                self.device_debugreg_input = value;
                Task::none()
            }
            Message::DeviceReadDebugReg => self.handle_device_read_debugreg(),
            Message::DeviceDebugRegRead(result) => self.handle_device_debugreg_read(result),
            Message::DeviceWriteDebugReg => self.handle_device_write_debugreg(),
            Message::DeviceDebugStreamStart => self.handle_device_debug_stream_start(),
            Message::DeviceDebugStreamStop => self.handle_device_debug_stream_stop(),
            Message::DeviceDebugStreamTick => {
                // Counters live in shared atomics; a redraw is enough.
                Task::none()
            }
            Message::DeviceDebugStreamSave => self.handle_device_debug_stream_save(),
            Message::DeviceDebugStreamSaved(result) => {
                self.handle_device_debug_stream_saved(result)
            }
            Message::DefaultSongDurationChanged(value) => {
                self.handle_default_song_duration_changed(value)
            }

            Message::FontSizeChanged(value) => self.handle_font_size_changed(value),
            // Starting directory settings
            Message::BrowseFileBrowserStartDir => Task::perform(
                async {
                    rfd::AsyncFileDialog::new()
                        .pick_folder()
                        .await
                        .map(|handle| handle.path().to_path_buf())
                },
                Message::FileBrowserStartDirSelected,
            ),

            Message::FileBrowserStartDirSelected(path) => {
                self.handle_file_browser_start_dir_selected(path)
            }

            Message::ClearFileBrowserStartDir => self.handle_clear_file_browser_start_dir(),

            Message::BrowseMusicPlayerStartDir => Task::perform(
                async {
                    rfd::AsyncFileDialog::new()
                        .pick_folder()
                        .await
                        .map(|handle| handle.path().to_path_buf())
                },
                Message::MusicPlayerStartDirSelected,
            ),

            Message::MusicPlayerStartDirSelected(path) => {
                self.handle_music_player_start_dir_selected(path)
            }

            Message::ClearMusicPlayerStartDir => self.handle_clear_music_player_start_dir(),
        }
    }

    fn view(&self, window_id: window::Id) -> Element<'_, Message> {
        // Check if this is the streaming window
        if Some(window_id) == self.streaming_window_id {
            return self
                .video_streaming
                .view_separate_window(self.settings.preferences.font_size)
                .map(Message::Streaming);
        }

        // If video is in fullscreen mode in main window, show only the fullscreen view
        if self.video_streaming.is_fullscreen {
            return self
                .video_streaming
                .view_fullscreen(self.settings.preferences.font_size)
                .map(Message::Streaming);
        }

        // Overwrite-confirmation dialog — shown when a local→remote copy would
        // clobber existing files on the device.
        if let Some(ref pending) = self.pending_copy {
            if !pending.conflicts.is_empty() {
                return self.view_overwrite_dialog(pending);
            }
        }

        // Drag-and-drop action dialog — shown when the OS dropped a file
        // on the window and the user hasn't picked an action yet.
        if let Some(ref dropped) = self.pending_drop {
            return self.view_drop_dialog(dropped);
        }

        // Eject A+B confirmation — guards against accidental clicks since
        // there's no undo for clearing a mounted disk.
        if self.pending_eject_confirm {
            return self.view_eject_confirm_dialog();
        }

        // Close-window confirmation — only shows when the user tried to
        // close mid-transfer.
        if self.pending_close.is_some() {
            return self.view_close_confirm_dialog();
        }

        // Help overlay (`?` keypress) — global cheatsheet, takes
        // precedence over the normal tab view so the user can pop it
        // open from anywhere.
        if self.show_help {
            return self.view_help_overlay();
        }

        // Tab bar with retro style
        let tabs = container(
            row![
                self.tab_button("FILE BROWSER", Tab::DualPaneBrowser),
                self.tab_button("MUSIC PLAYER", Tab::MusicPlayer),
                self.tab_button("VIDEO VIEWER", Tab::VideoViewer),
                self.tab_button("MEMORY", Tab::MemoryEditor),
                self.tab_button("MONITOR", Tab::Monitor),
                self.tab_button("CONFIG", Tab::Configuration),
                // PROFILES tab hidden — feature is WIP. Re-enable when ready.
                // self.tab_button("PROFILES", Tab::Profiles),
                self.tab_button("ASSEMBLY64", Tab::Assembly64),
                self.tab_button("BASIC", Tab::BasicEditor),
                self.tab_button("DEVICE", Tab::Device),
                self.tab_button("SETTINGS", Tab::Settings),
            ]
            .spacing(2),
        )
        .padding(5);

        // Connection status bar at top
        let connection_bar = self.view_connection_bar();

        // Main content area
        let content = container(match self.active_tab {
            Tab::DualPaneBrowser => self.view_dual_pane_browser(),
            Tab::MusicPlayer => self.view_music_player(),
            Tab::VideoViewer => {
                if self.streaming_window_id.is_some() {
                    // Streaming is shown in separate window - show placeholder
                    let fs =
                        crate::styles::FontSizes::from_base(self.settings.preferences.font_size);
                    container(
                        column![
                            text("Video is displayed in separate window").size(fs.large + 2),
                            Space::new(),
                            button(text("Close Separate Window").size(fs.normal))
                                .on_press(Message::CloseStreamingWindow)
                                .padding([8, 16]),
                        ]
                        .align_x(iced::Alignment::Center)
                        .spacing(10),
                    )
                    .width(Length::Fill)
                    .height(Length::Fill)
                    .center_x(Length::Fill)
                    .center_y(Length::Fill)
                    .into()
                } else {
                    self.video_streaming
                        .view(self.settings.preferences.font_size)
                        .map(Message::Streaming)
                }
            }
            Tab::MemoryEditor => self
                .memory_editor
                .view(self.status.connected, self.settings.preferences.font_size)
                .map(Message::MemoryEditor),
            Tab::Monitor => self
                .sid_monitor
                .view(self.status.connected, self.settings.preferences.font_size)
                .map(Message::Monitor),
            Tab::Configuration => self
                .config_editor
                .view(self.status.connected, self.settings.preferences.font_size)
                .map(Message::ConfigEditor),
            Tab::Profiles => self
                .device_profile_manager
                .view(self.status.connected, self.settings.preferences.font_size)
                .map(Message::DeviceProfileManager),
            Tab::Assembly64 => self
                .assembly64_browser
                .view(self.settings.preferences.font_size, self.status.connected)
                .map(Message::Assembly64Browser),
            Tab::BasicEditor => self
                .basic_editor
                .view(self.settings.preferences.font_size, self.status.connected)
                .map(Message::BasicEditor),
            Tab::Device => self.view_device(),
            Tab::Settings => self.view_settings(),
        })
        .padding(10)
        .width(Length::Fill)
        .height(Length::Fill);

        // Bottom status/control bar
        let status_bar = self.view_status_bar();

        let main_content = column![
            connection_bar,
            rule::horizontal(1),
            tabs,
            rule::horizontal(1),
            content,
            rule::horizontal(1),
            status_bar
        ]
        .spacing(0);

        let body: Element<'_, Message> = container(main_content)
            .width(Length::Fill)
            .height(Length::Fill)
            .into();

        // Layer a transient toast on top when one is active.
        if let Some((msg, _)) = &self.toast {
            iced::widget::stack![body, self.view_toast(msg)].into()
        } else {
            body
        }
    }

    /// Bottom-centered banner used by [`show_toast`]. Pointer-transparent
    /// (no `on_press`), so clicks still reach the underlying UI.
    fn view_toast<'a>(&self, message: &'a str) -> Element<'a, Message> {
        let fs = crate::styles::FontSizes::from_base(self.settings.preferences.font_size);
        let banner = container(text(message).size(fs.normal))
            .padding([10, 20])
            .style(|theme: &Theme| {
                let palette = theme.extended_palette();
                container::Style {
                    background: Some(palette.success.base.color.into()),
                    text_color: Some(palette.success.base.text),
                    border: iced::Border {
                        radius: 6.0.into(),
                        ..Default::default()
                    },
                    ..Default::default()
                }
            });
        container(banner)
            .width(Length::Fill)
            .height(Length::Fill)
            .padding(iced::Padding::ZERO.bottom(40))
            .center_x(Length::Fill)
            .align_y(iced::alignment::Vertical::Bottom)
            .into()
    }
    fn subscription(&self) -> Subscription<Message> {
        use iced::event::{self, Event};
        use iced::keyboard::{self, Key};
        use std::time::Duration;

        // If main window is closed, stop all subscriptions to allow clean exit
        if self.main_window_id.is_none() {
            return Subscription::none();
        }

        // Keyboard shortcuts: ESC to exit fullscreen, Opt+F (macOS) or Alt+F (Windows/Linux) to toggle
        let keyboard_sub = event::listen_with(|event, status, _id| {
            if let Event::Keyboard(keyboard::Event::KeyPressed { key, modifiers, .. }) = event {
                // `captured` is true when a focused widget (e.g. a text
                // input) consumed the key. Pane-navigation bindings check
                // this to avoid stealing keystrokes from text fields —
                // typing in the filter box shouldn't move the file cursor.
                let captured = matches!(status, event::Status::Captured);
                match key {
                    Key::Named(keyboard::key::Named::Escape) => Some(Message::EscPressed),
                    // `?` opens the Help overlay (cheat-sheet of all keybinds).
                    // Match both forms iced may report on Shift+/ : the
                    // already-normalised `"?"` AND the raw `"/"` with the
                    // shift modifier — platforms differ on which one fires.
                    // No `!captured` gate here: matches the pattern of every
                    // other shortcut in this list, otherwise the binding
                    // silently dies whenever any widget has focus.
                    Key::Character(ref c)
                        if c.as_str() == "?" || (c.as_str() == "/" && modifiers.shift()) =>
                    {
                        Some(Message::ShowHelp)
                    }
                    Key::Character(ref c) if c.as_str() == "f" && modifiers.alt() => Some(
                        Message::Streaming(streaming::StreamingMessage::ToggleFullscreen),
                    ),
                    // Cmd/Ctrl+Z = Undo, Cmd/Ctrl+Shift+Z = Redo in Memory Editor
                    Key::Character(ref c)
                        if c.as_str() == "z" && modifiers.command() && !modifiers.shift() =>
                    {
                        Some(Message::MemoryEditor(
                            memory_editor::MemoryEditorMessage::Undo,
                        ))
                    }
                    Key::Character(ref c)
                        if c.as_str() == "z" && modifiers.command() && modifiers.shift() =>
                    {
                        Some(Message::MemoryEditor(
                            memory_editor::MemoryEditorMessage::Redo,
                        ))
                    }
                    // File browser shortcuts (TC-style)
                    Key::Named(keyboard::key::Named::F2) => Some(Message::FnRename),
                    Key::Named(keyboard::key::Named::F3) => Some(Message::FnView),
                    Key::Named(keyboard::key::Named::F4) => Some(Message::FnEdit),
                    // Space on a folder calculates its recursive size, just
                    // like Total Commander. Plain Space only — modifiers
                    // would interfere with text input shortcuts.
                    Key::Named(keyboard::key::Named::Space) if modifiers.is_empty() => {
                        Some(Message::FnSize)
                    }
                    Key::Named(keyboard::key::Named::F5) => Some(Message::FnCopy),
                    Key::Named(keyboard::key::Named::F7) => Some(Message::FnMkDir),
                    Key::Named(keyboard::key::Named::F8) => Some(Message::FnDelete),
                    Key::Named(keyboard::key::Named::Tab) if !modifiers.shift() => {
                        Some(Message::ToggleActivePane)
                    }
                    Key::Named(keyboard::key::Named::Backspace) => {
                        Some(Message::NavigateUpActivePane)
                    }
                    // Cmd/Ctrl+Left = go up one folder (back to parent).
                    // Also Cmd/Ctrl+Up — Mac Finder muscle memory. Both
                    // mirror the existing Backspace binding.
                    Key::Named(keyboard::key::Named::ArrowLeft) if modifiers.command() => {
                        Some(Message::NavigateUpActivePane)
                    }
                    Key::Named(keyboard::key::Named::ArrowUp) if modifiers.command() => {
                        Some(Message::NavigateUpActivePane)
                    }
                    Key::Character(ref c) if c.as_str() == "a" && modifiers.command() => {
                        Some(Message::SelectAllActivePane)
                    }
                    // Refresh active pane — Ctrl/Cmd+R, matches Total
                    // Commander's "Re-read source" muscle memory.
                    Key::Character(ref c) if c.as_str() == "r" && modifiers.command() => {
                        Some(Message::FnRefresh)
                    }
                    // Ctrl/Cmd+L — focus the active pane's path field.
                    // Universal "focus address bar" convention from web
                    // browsers and modern file managers.
                    Key::Character(ref c) if c.as_str() == "l" && modifiers.command() => {
                        Some(Message::FocusPathField)
                    }
                    // ── Pane navigation — gated by `!captured` so text
                    // inputs (filter, path, dialog fields) still get their
                    // keystrokes first.
                    Key::Named(keyboard::key::Named::ArrowUp) if !captured => {
                        Some(Message::PaneCursorUp)
                    }
                    Key::Named(keyboard::key::Named::ArrowDown) if !captured => {
                        Some(Message::PaneCursorDown)
                    }
                    Key::Named(keyboard::key::Named::Enter) if !captured => {
                        Some(Message::PaneActivate)
                    }
                    // Quick search: any single printable alphanumeric pressed
                    // with no modifiers, when no widget grabbed it. Filters
                    // out punctuation/whitespace so symbols don't poison the
                    // search buffer.
                    Key::Character(ref c)
                        if !captured
                            && modifiers.is_empty()
                            && c.chars().count() == 1
                            && c.chars()
                                .next()
                                .map_or(false, |ch| ch.is_ascii_alphanumeric()) =>
                    {
                        c.chars().next().map(Message::PaneQuickSearch)
                    }
                    _ => None,
                }
            } else {
                None
            }
        });

        // Window-level event listener: streaming-window close + OS file drops.
        let window_events = iced::event::listen_with(|event, _status, id| match event {
            iced::Event::Window(iced::window::Event::Closed) => Some(Message::WindowClosed(id)),
            iced::Event::Window(iced::window::Event::CloseRequested) => {
                Some(Message::WindowCloseRequested(id))
            }
            iced::Event::Window(iced::window::Event::FileDropped(path)) => {
                Some(Message::FileDropped(path))
            }
            _ => None,
        });

        // Periodic connection check every 60 seconds
        // Suppressed while transferring or applying profiles to avoid
        // overwhelming the device's HTTP server
        let status_check = if self.remote_browser.is_transferring()
            || self.device_profile_manager.is_loading
        {
            // Suppress all polling while transferring or applying profiles
            // to avoid overwhelming the device's HTTP server.
            Subscription::none()
        } else if self.status.connected {
            // Connected — poll at the slow background cadence, but switch
            // to a fast retry during a transient outage window so a 2-3s
            // reboot recovers before the user notices.
            let interval_secs = if self.consecutive_status_failures > 0 {
                STATUS_POLL_RETRY_SECS
            } else {
                STATUS_POLL_NORMAL_SECS
            };
            iced::time::every(Duration::from_secs(interval_secs)).map(|_| Message::RefreshStatus)
        } else if self.connection.is_some() {
            // Officially disconnected but the user is still authenticated —
            // keep tapping at the device so we silently reconnect when it
            // comes back online (e.g. after a longer reboot).
            iced::time::every(Duration::from_secs(STATUS_POLL_RECONNECT_SECS))
                .map(|_| Message::RefreshStatus)
        } else {
            Subscription::none()
        };

        // Copy progress tick - poll every 250ms while a copy is in progress
        let copy_progress_tick = {
            let has_progress = self
                .copy_progress
                .lock()
                .ok()
                .map(|g| g.is_some())
                .unwrap_or(false);
            if has_progress {
                iced::time::every(Duration::from_millis(250)).map(|_| Message::CopyProgressTick)
            } else {
                Subscription::none()
            }
        };

        let toast_tick = if self.toast.is_some() {
            iced::time::every(Duration::from_millis(500)).map(|_| Message::ToastTick)
        } else {
            Subscription::none()
        };

        // Refresh the debug-stream capture counters while active.
        let debug_stream_tick = if self.debug_stream.active {
            iced::time::every(Duration::from_millis(500)).map(|_| Message::DeviceDebugStreamTick)
        } else {
            Subscription::none()
        };

        Subscription::batch([
            self.video_streaming.subscription().map(Message::Streaming),
            self.music_player.subscription().map(Message::MusicPlayer),
            self.remote_browser
                .subscription()
                .map(Message::RemoteBrowser),
            self.memory_editor.subscription().map(Message::MemoryEditor),
            self.sid_monitor.subscription().map(Message::Monitor),
            keyboard_sub,
            window_events,
            status_check,
            copy_progress_tick,
            toast_tick,
            debug_stream_tick,
        ])
    }

    fn tab_button<'a>(&self, label: &'a str, tab: Tab) -> Element<'a, Message> {
        let fs = crate::styles::FontSizes::from_base(self.settings.preferences.font_size);
        let is_active = self.active_tab == tab;
        button(text(label).size(fs.large))
            .on_press(Message::TabSelected(tab))
            .padding([8, 16])
            .style(if is_active {
                button::primary
            } else {
                button::secondary
            })
            .into()
    }

    /// Centered modal dialog shown when the OS dropped a file on our window.
    /// Renders a button per applicable action (Run / Mount / Open / Upload)
    /// with the always-available Cancel.
    /// Route a per-pane file-browser message to the active pane. Used by
    /// keyboard nav (arrows, Enter, quick-search) and folder-size hotkeys
    /// — anything that only acts on the local pane. No-op when:
    /// - the dual-pane browser isn't the active tab (so typing in BASIC /
    ///   Memory / Assembly64 doesn't poison the local browser's state),
    /// - the active pane is Right (remote-side keyboard nav not wired —
    ///   different message enum, slower I/O on activate).
    /// Show a transient banner at the bottom of the window. Subscription
    /// ticks every 500ms while one is active and clears it after 4s.
    fn show_toast(&mut self, message: impl Into<String>) {
        self.toast = Some((message.into(), std::time::Instant::now()));
    }

    /// True when any long-running transfer is in flight — used to gate the
    /// window-close confirmation. BASIC send isn't included: it's fast and
    /// already has its own Cancel button.
    fn is_transfer_in_flight(&self) -> bool {
        let copy_in_progress = self
            .copy_progress
            .lock()
            .ok()
            .map(|g| g.is_some())
            .unwrap_or(false);
        self.drop_in_flight
            || self.remote_browser.is_transferring()
            || self.assembly64_browser.is_busy()
            || copy_in_progress
    }

    fn dispatch_local_pane_message(&mut self, msg: FileBrowserMessage) -> Task<Message> {
        if self.active_tab != Tab::DualPaneBrowser {
            return Task::none();
        }
        let ctx = self.tab_context();
        match self.active_pane {
            Pane::Left => self.left_browser.update(msg, ctx).map(Message::LeftBrowser),
            Pane::Right => Task::none(),
        }
    }

    /// Centered modal cheatsheet of every app-level keybind. Triggered by
    /// `?`; dismissed by Esc or the Close button. Edit `HELP_BINDS` to add
    /// new entries — keeping them in one table avoids documentation rot.

    /// Confirmation modal for the Eject A+B toolbar button. Mirrors the
    /// drop-dialog overlay pattern so click-outside / Esc dismiss for free.

    /// "Really close?" prompt shown when the user tried to quit while a
    /// transfer was in flight. Same overlay pattern as the other modals so
    /// click-outside cancels.

    fn view_music_player(&self) -> Element<'_, Message> {
        self.music_player
            .view(self.settings.preferences.font_size)
            .map(Message::MusicPlayer)
    }

    /// Debug bus-trace stream capture controls (part of the DEVICE tab).
    fn view_debug_stream_section(&self) -> Element<'_, Message> {
        let fs = crate::styles::FontSizes::from_base(self.settings.preferences.font_size);
        let connected = self.status.connected;
        let active = self.debug_stream.active;

        let captured_kib = self.debug_stream.captured_len() as f64 / 1024.0;
        let stats = text(format!(
            "{} · {} packets · {:.1} KiB captured",
            if active { "CAPTURING" } else { "idle" },
            self.debug_stream.packets(),
            captured_kib,
        ))
        .size(fs.small)
        .color(iced::Color::from_rgb(0.6, 0.7, 0.8));

        let start_stop = if active {
            button(text("Stop").size(fs.small))
                .on_press(Message::DeviceDebugStreamStop)
                .padding([4, 10])
                .style(crate::styles::action_button)
        } else {
            button(text("Start Capture").size(fs.small))
                .on_press_maybe(connected.then_some(Message::DeviceDebugStreamStart))
                .padding([4, 10])
                .style(crate::styles::action_button)
        };

        let save_btn = button(text("Save Capture…").size(fs.small))
            .on_press_maybe(
                (!active && self.debug_stream.captured_len() > 0)
                    .then_some(Message::DeviceDebugStreamSave),
            )
            .padding([4, 10])
            .style(crate::styles::nav_button);

        column![
            text("DEBUG BUS-TRACE STREAM (U64 only)")
                .size(fs.normal)
                .color(iced::Color::from_rgb(0.7, 0.72, 0.8)),
            row![start_stop, save_btn, Space::new().width(12), stats]
                .spacing(8)
                .align_y(iced::Alignment::Center),
            text(
                "Cycle-accurate 6510/VIC/1541 bus trace. Mutually exclusive with the \
                 VIC video stream. Saved as a raw capture (.bin) for the documented \
                 GtkWave/VCD converter — the sample layout is FPGA-defined and not \
                 decoded here."
            )
            .size(fs.tiny)
            .color(iced::Color::from_rgb(0.55, 0.55, 0.6)),
        ]
        .spacing(8)
        .into()
    }

    /// DEVICE tab — drive control, keyboard injection, and the debug
    /// register. Groups low-level device capabilities the REST API exposes
    /// but that don't fit the file/music/config tabs.
    fn view_device(&self) -> Element<'_, Message> {
        let fs = crate::styles::FontSizes::from_base(self.settings.preferences.font_size);
        let connected = self.status.connected;

        // One drive's control cluster: mode switch + power + reset.
        let drive_row = |label: &'static str, drive: &'static str| -> Element<'_, Message> {
            let mode_btn =
                |mode: &'static str| {
                    button(text(mode).size(fs.small))
                        .on_press_maybe(connected.then(|| {
                            Message::DeviceSetDriveMode(drive.to_string(), mode.to_string())
                        }))
                        .padding([3, 8])
                        .style(crate::styles::nav_button)
                };
            row![
                text(label).size(fs.normal).width(Length::Fixed(70.0)),
                text("Mode:").size(fs.small),
                mode_btn("1541"),
                mode_btn("1571"),
                mode_btn("1581"),
                Space::new().width(12),
                button(text("Power On").size(fs.small))
                    .on_press_maybe(
                        connected.then(|| Message::DeviceDrivePower(drive.to_string(), true)),
                    )
                    .padding([3, 8])
                    .style(crate::styles::nav_button),
                button(text("Power Off").size(fs.small))
                    .on_press_maybe(
                        connected.then(|| Message::DeviceDrivePower(drive.to_string(), false)),
                    )
                    .padding([3, 8])
                    .style(crate::styles::nav_button),
                button(text("Reset").size(fs.small))
                    .on_press_maybe(connected.then(|| Message::DeviceDriveReset(drive.to_string())))
                    .padding([3, 8])
                    .style(crate::styles::nav_button),
            ]
            .spacing(6)
            .align_y(iced::Alignment::Center)
            .into()
        };

        let drive_section = column![
            text("DRIVE CONTROL")
                .size(fs.normal)
                .color(iced::Color::from_rgb(0.7, 0.72, 0.8)),
            drive_row("Drive A", "a"),
            drive_row("Drive B", "b"),
        ]
        .spacing(8);

        // Keyboard injection — type into the running C64 (BASIC or any prompt).
        let keyboard_section = column![
            text("KEYBOARD INPUT (KERNAL buffer)")
                .size(fs.normal)
                .color(iced::Color::from_rgb(0.7, 0.72, 0.8)),
            row![
                text_input("text to type into the C64…", &self.device_keyboard_input)
                    .on_input(Message::DeviceKeyboardInputChanged)
                    .on_submit(Message::DeviceSendKeys)
                    .size(fs.small)
                    .padding(4)
                    .width(Length::Fixed(360.0)),
                button(text("Send Keys").size(fs.small))
                    .on_press_maybe(connected.then_some(Message::DeviceSendKeys))
                    .padding([4, 10])
                    .style(crate::styles::action_button),
            ]
            .spacing(6)
            .align_y(iced::Alignment::Center),
            text(
                "Feeds text into the KERNAL keyboard buffer ($0277) and presses RETURN — \
                  works at the BASIC prompt and programs that read via the KERNAL (GETIN). \
                  Games that scan the keyboard matrix ($DC00) directly won't see it."
            )
            .size(fs.tiny)
            .color(iced::Color::from_rgb(0.55, 0.55, 0.6)),
        ]
        .spacing(8);

        // Debug register ($D7FF) — U64 only.
        let debugreg_current = match self.device_debugreg_value {
            Some(v) => format!("${:02X} ({})", v, v),
            None => "—".to_string(),
        };
        let debugreg_section = column![
            text("DEBUG REGISTER ($D7FF · U64 only)")
                .size(fs.normal)
                .color(iced::Color::from_rgb(0.7, 0.72, 0.8)),
            row![
                button(text("Read").size(fs.small))
                    .on_press_maybe(connected.then_some(Message::DeviceReadDebugReg))
                    .padding([4, 10])
                    .style(crate::styles::nav_button),
                text(format!("Current: {}", debugreg_current)).size(fs.small),
                Space::new().width(16),
                text("Write hex:").size(fs.small),
                text_input("00", &self.device_debugreg_input)
                    .on_input(Message::DeviceDebugRegInputChanged)
                    .on_submit(Message::DeviceWriteDebugReg)
                    .size(fs.small)
                    .padding(4)
                    .width(Length::Fixed(70.0)),
                button(text("Write").size(fs.small))
                    .on_press_maybe(connected.then_some(Message::DeviceWriteDebugReg))
                    .padding([4, 10])
                    .style(crate::styles::action_button),
            ]
            .spacing(6)
            .align_y(iced::Alignment::Center),
        ]
        .spacing(8);

        let gate = if connected {
            text("")
        } else {
            text("Not connected — device controls are disabled.")
                .size(fs.small)
                .color(iced::Color::from_rgb(0.8, 0.4, 0.0))
        };

        let content = column![
            text("DEVICE CONTROLS").size(fs.large),
            gate,
            rule::horizontal(1),
            drive_section,
            rule::horizontal(1),
            keyboard_section,
            rule::horizontal(1),
            debugreg_section,
            rule::horizontal(1),
            self.view_debug_stream_section(),
        ]
        .spacing(16)
        .padding(10);

        scrollable(content).height(Length::Fill).into()
    }

    fn establish_connection(&mut self) {
        self.connection = None;
        self.host_url = None;
        self.status.connected = false;
        self.status.device_info = None;
        self.status.mounted_disks.clear();

        if self.settings.connection.host.is_empty() {
            log::error!("Host IP is empty");
            self.user_message = Some(UserMessage::Error("Host IP cannot be empty".to_string()));
            return;
        }

        log::info!(
            "Attempting to connect to: {}",
            self.settings.connection.host
        );

        // Parse host
        let host = if let Ok(ip_addr) = self.settings.connection.host.parse::<std::net::Ipv4Addr>()
        {
            log::debug!("Parsed as IPv4: {}", ip_addr);
            Host::Ipv4(ip_addr)
        } else if let Ok(ip_addr) = self.settings.connection.host.parse::<std::net::Ipv6Addr>() {
            log::debug!("Parsed as IPv6: {}", ip_addr);
            Host::Ipv6(ip_addr)
        } else {
            match Host::parse(&self.settings.connection.host) {
                Ok(h) => {
                    log::debug!("Parsed as domain: {:?}", h);
                    h
                }
                Err(e) => {
                    log::error!(
                        "Failed to parse host '{}': {}",
                        self.settings.connection.host,
                        e
                    );
                    self.user_message = Some(UserMessage::Error(format!("Invalid host: {}", e)));
                    return;
                }
            }
        };

        log::info!("Creating REST connection...");
        match Rest::new(&host, self.settings.connection.password.clone()) {
            Ok(rest) => {
                log::info!("REST client created, verifying connection...");
                self.connection = Some(Arc::new(Mutex::new(rest)));
                // Build proper HTTP URL
                let http_url = format!("http://{}", self.settings.connection.host);
                self.host_url = Some(http_url.clone());
                // Don't set connected = true here - let StatusUpdated verify the connection
                self.remote_browser
                    .set_host(Some(http_url), self.settings.connection.password.clone());
                // Set telnet host for video streaming control
                self.video_streaming
                    .set_ultimate_host(Some(self.settings.connection.host.clone()));
                self.user_message = Some(UserMessage::Info(format!(
                    "Connecting to {}...",
                    self.settings.connection.host
                )));
            }
            Err(e) => {
                log::error!("Connection failed: {}", e);
                self.user_message = Some(UserMessage::Error(format!("Connection failed: {}", e)));
            }
        }
    }
}

async fn execute_template_commands(
    connection: Arc<Mutex<Rest>>,
    commands: Vec<String>,
) -> Result<(), String> {
    for command in commands {
        log::info!("Executing: {}", command);
        let conn = connection.clone();

        if command.starts_with("RESET") {
            let result = tokio::time::timeout(
                tokio::time::Duration::from_secs(REST_TIMEOUT_SECS),
                tokio::task::spawn_blocking(move || {
                    let conn = conn.lock().unwrap();
                    conn.reset().map_err(|e| format!("Reset failed: {}", e))
                }),
            )
            .await;

            match result {
                Ok(Ok(r)) => r?,
                Ok(Err(e)) => return Err(format!("Task error: {}", e)),
                Err(_) => return Err("Reset timed out - device may be offline".to_string()),
            }
            tokio::time::sleep(tokio::time::Duration::from_secs(3)).await;
        } else if let Some(text) = command.strip_prefix("TYPE ") {
            let text = text.to_string();
            let result = tokio::time::timeout(
                tokio::time::Duration::from_secs(REST_TIMEOUT_SECS),
                tokio::task::spawn_blocking(move || {
                    let conn = conn.lock().unwrap();
                    conn.type_text(&text)
                        .map_err(|e| format!("Type failed: {}", e))
                }),
            )
            .await;

            match result {
                Ok(Ok(r)) => r?,
                Ok(Err(e)) => return Err(format!("Task error: {}", e)),
                Err(_) => return Err("Type command timed out - device may be offline".to_string()),
            }
            tokio::time::sleep(tokio::time::Duration::from_millis(200)).await;
        } else if command.starts_with("LOAD") {
            let result = tokio::time::timeout(
                tokio::time::Duration::from_secs(REST_TIMEOUT_SECS),
                tokio::task::spawn_blocking(move || {
                    let conn = conn.lock().unwrap();
                    conn.type_text("load\"*\",8,1\n")
                        .map_err(|e| format!("Load failed: {}", e))
                }),
            )
            .await;

            match result {
                Ok(Ok(r)) => r?,
                Ok(Err(e)) => return Err(format!("Task error: {}", e)),
                Err(_) => return Err("Load command timed out - device may be offline".to_string()),
            }
            tokio::time::sleep(tokio::time::Duration::from_millis(200)).await;
        } else if command.starts_with("RUN") {
            let result = tokio::time::timeout(
                tokio::time::Duration::from_secs(REST_TIMEOUT_SECS),
                tokio::task::spawn_blocking(move || {
                    let conn = conn.lock().unwrap();
                    conn.type_text("run\n")
                        .map_err(|e| format!("Run failed: {}", e))
                }),
            )
            .await;

            match result {
                Ok(Ok(r)) => r?,
                Ok(Err(e)) => return Err(format!("Task error: {}", e)),
                Err(_) => return Err("Run command timed out - device may be offline".to_string()),
            }
        }
    }

    Ok(())
}

async fn fetch_status(connection: Arc<Mutex<Rest>>) -> Result<StatusInfo, String> {
    // Use spawn_blocking to avoid runtime conflicts with ultimate64 crate
    // Wrap in timeout to prevent hangs when device is offline
    let result = tokio::time::timeout(
        tokio::time::Duration::from_secs(REST_TIMEOUT_SECS),
        tokio::task::spawn_blocking(move || {
            let conn = connection.lock().unwrap();

            let device_info = match conn.info() {
                Ok(info) => Some(format!("{} ({})", info.product, info.firmware_version)),
                Err(e) => return Err(format!("Failed to get device info: {}", e)),
            };

            let mounted_disks = match conn.drive_list() {
                Ok(drives) => drives
                    .into_iter()
                    .filter_map(|(name, drive)| drive.image_file.map(|file| (name, file)))
                    .collect(),
                Err(_) => Vec::new(),
            };

            Ok(StatusInfo {
                connected: true,
                device_info,
                mounted_disks,
            })
        }),
    )
    .await;

    match result {
        Ok(Ok(status)) => status,
        Ok(Err(e)) => Err(format!("Task error: {}", e)),
        Err(_) => Err("Connection timed out - device may be offline".to_string()),
    }
}

impl Drop for Ultimate64Browser {
    fn drop(&mut self) {
        log::info!("Application dropping, cleaning up...");

        self.video_streaming
            .stop_signal
            .store(true, std::sync::atomic::Ordering::Relaxed);

        // Reset C64 if music was playing (with timeout to prevent hang)
        if self.music_player.playback_state == PlaybackState::Playing {
            if let Some(conn) = &self.connection {
                log::info!("Resetting C64 on drop...");
                let conn = conn.clone();

                let (tx, rx) = std::sync::mpsc::channel();

                std::thread::spawn(move || {
                    let result = if let Ok(c) = conn.try_lock() {
                        c.reset().map_err(|e| e.to_string())
                    } else {
                        Err("Could not acquire lock".to_string())
                    };
                    let _ = tx.send(result);
                });

                match rx.recv_timeout(std::time::Duration::from_secs(1)) {
                    Ok(Ok(())) => log::info!("C64 reset successful"),
                    Ok(Err(e)) => log::warn!("C64 reset failed: {}", e),
                    Err(_) => log::warn!("C64 reset timed out - device may be offline"),
                }
            }
        }
    }
}

// ── FTP helper functions for copy operations ─────────────────────────────────

/// Recursively count files in a remote directory via FTP LIST
fn count_remote_files_recursive(ftp: &mut suppaftp::FtpStream, remote_path: &str) -> usize {
    let entries = match ftp.list(Some(remote_path)) {
        Ok(e) => e,
        Err(_) => return 0,
    };

    let mut count = 0;
    for line in &entries {
        let parts: Vec<&str> = line.split_whitespace().collect();
        if parts.len() < 9 {
            continue;
        }
        let name = parts[8..].join(" ");
        if name == "." || name == ".." {
            continue;
        }
        let is_dir = line.starts_with('d');
        if is_dir {
            let child = format!("{}/{}", remote_path.trim_end_matches('/'), name);
            count += count_remote_files_recursive(ftp, &child);
        } else {
            count += 1;
        }
    }
    count
}

/// Download a remote directory recursively, updating shared progress per file
fn download_directory_with_progress(
    ftp: &mut suppaftp::FtpStream,
    remote_path: &str,
    local_path: &std::path::Path,
    progress: &std::sync::Arc<std::sync::Mutex<Option<crate::ftp_ops::TransferProgress>>>,
    downloaded: &mut usize,
) -> Result<usize, String> {
    use std::io::Read;

    std::fs::create_dir_all(local_path)
        .map_err(|e| format!("Create dir {}: {}", local_path.display(), e))?;

    let entries = ftp
        .list(Some(remote_path))
        .map_err(|e| format!("List {}: {}", remote_path, e))?;

    let mut files_count = 0;

    for entry_line in &entries {
        // Check cancellation
        let is_cancelled = progress
            .lock()
            .ok()
            .and_then(|g| g.as_ref().map(|p| p.cancelled))
            .unwrap_or(false);
        if is_cancelled {
            break;
        }

        let parts: Vec<&str> = entry_line.split_whitespace().collect();
        if parts.len() < 9 {
            continue;
        }
        let name = parts[8..].join(" ");
        if name == "." || name == ".." {
            continue;
        }

        let is_dir = entry_line.starts_with('d');
        let child_remote = format!("{}/{}", remote_path.trim_end_matches('/'), name);
        let child_local = local_path.join(&name);

        if is_dir {
            match download_directory_with_progress(
                ftp,
                &child_remote,
                &child_local,
                progress,
                downloaded,
            ) {
                Ok(f) => files_count += f,
                Err(e) => log::warn!("Skip dir {}: {}", child_remote, e),
            }
        } else {
            if let Ok(mut g) = progress.lock() {
                if let Some(ref mut p) = *g {
                    p.current_file = name.clone();
                }
            }

            match ftp.retr_as_stream(&child_remote) {
                Ok(mut reader) => {
                    let mut data = Vec::new();
                    if reader.read_to_end(&mut data).is_ok() {
                        let _ = ftp.finalize_retr_stream(reader);
                        if std::fs::write(&child_local, &data).is_ok() {
                            files_count += 1;
                            *downloaded += 1;
                            if let Ok(mut g) = progress.lock() {
                                if let Some(ref mut p) = *g {
                                    p.current = *downloaded;
                                    p.bytes_transferred += data.len() as u64;
                                }
                            }
                        }
                    }
                }
                Err(e) => {
                    log::warn!("Skip file {}: {}", child_remote, e);
                }
            }
        }
    }

    Ok(files_count)
}

#[cfg(test)]
mod tests {
    use super::DropAction;
    use std::path::PathBuf;

    fn p(name: &str) -> PathBuf {
        PathBuf::from(name)
    }

    #[test]
    fn drop_actions_for_prg_offers_run() {
        let actions = DropAction::available_for("prg", &p("game.prg"));
        assert!(
            matches!(actions[0], DropAction::RunOnDevice { runner, .. } if runner == "run_prg")
        );
    }

    #[test]
    fn drop_actions_for_crt_offers_run_crt() {
        let actions = DropAction::available_for("crt", &p("cart.crt"));
        assert!(
            matches!(actions[0], DropAction::RunOnDevice { runner, .. } if runner == "run_crt")
        );
    }

    #[test]
    fn drop_actions_for_sid_offers_sidplay() {
        let actions = DropAction::available_for("sid", &p("song.sid"));
        assert!(
            matches!(actions[0], DropAction::RunOnDevice { runner, .. } if runner == "sidplay")
        );
    }

    #[test]
    fn drop_actions_for_disk_offers_mount() {
        for ext in ["d64", "d71", "d81", "g64", "g71"] {
            let actions = DropAction::available_for(ext, &p(&format!("disk.{}", ext)));
            assert!(
                matches!(actions[0], DropAction::MountDisk { .. }),
                "ext {} expected MountDisk",
                ext
            );
        }
    }

    #[test]
    fn drop_actions_for_bas_offers_open_in_editor() {
        let actions = DropAction::available_for("bas", &p("hello.bas"));
        assert!(matches!(actions[0], DropAction::OpenInBasicEditor { .. }));
    }

    #[test]
    fn drop_actions_for_unknown_extension_is_empty() {
        // The view appends Upload-to-remote + Cancel itself; the per-type
        // list stays empty for unknown types so the dialog isn't cluttered.
        let actions = DropAction::available_for("zip", &p("foo.zip"));
        assert!(actions.is_empty());
        let actions = DropAction::available_for("", &p("README"));
        assert!(actions.is_empty());
    }

    #[test]
    fn drop_action_status_labels_are_unique() {
        // Sanity: each action has a distinct human label so the status bar
        // is meaningful regardless of which one fired.
        let labels: Vec<String> = vec![
            DropAction::RunOnDevice {
                path: p("a"),
                runner: "run_prg",
            }
            .status_label(),
            DropAction::MountDisk { path: p("a") }.status_label(),
            DropAction::OpenInBasicEditor { path: p("a") }.status_label(),
            DropAction::UploadToRemote { path: p("a") }.status_label(),
        ];
        let mut sorted = labels.clone();
        sorted.sort();
        sorted.dedup();
        assert_eq!(sorted.len(), labels.len(), "status labels must be unique");
    }
}
