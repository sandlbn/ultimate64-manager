// Remove the windows_subsystem attribute during development to see console output
// Uncomment for release builds:
#![cfg_attr(target_os = "windows", windows_subsystem = "windows")]

use iced::{
    widget::{
        button, column, container, pick_list, progress_bar, row, rule, scrollable, text,
        text_input, tooltip, Space,
    },
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
use tokio::sync::Mutex as TokioMutex;
use ultimate64::Rest;
use url::Host;
use version_check::{NewVersionInfo, VersionCheckMessage};

mod api;
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
use settings::{AppSettings, ConnectionSettings, StreamControlMethod};
use sid_monitor::{SidMonitor, SidMonitorMessage};
use streaming::{StreamingMessage, VideoStreaming};
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
    connection: Option<Arc<TokioMutex<Rest>>>,
    host_url: Option<String>, // Store host URL for direct HTTP requests
    status: StatusInfo,
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

    fn update(&mut self, message: Message) -> Task<Message> {
        match message {
            Message::SaveProfile => {
                // Sync current input fields to the active profile before saving
                let conn_settings = ConnectionSettings {
                    host: self.host_input.clone(),
                    password: if self.password_input.is_empty() {
                        None
                    } else {
                        Some(self.password_input.clone())
                    },
                    stream_control_method: self.settings.connection.stream_control_method,
                };
                self.profile_manager.active_settings_mut().connection = conn_settings;

                if let Ok(size) = self.font_size_input.parse::<u32>() {
                    if size >= 8 && size <= 24 {
                        self.profile_manager
                            .active_settings_mut()
                            .preferences
                            .font_size = size;
                    }
                }

                self.settings = self.profile_manager.active_settings().clone();

                match self.profile_manager.save() {
                    Ok(()) => {
                        self.user_message =
                            Some(UserMessage::Info("Profile saved successfully".to_string()));
                    }
                    Err(e) => {
                        self.user_message =
                            Some(UserMessage::Error(format!("Failed to save profile: {}", e)));
                    }
                }
                Task::none()
            }
            Message::StartDiscovery => {
                if self.is_discovering {
                    return Task::none();
                }
                self.is_discovering = true;
                self.discovered_devices.clear();
                self.user_message = Some(UserMessage::Info("Scanning network...".to_string()));

                Task::perform(discovery::discover_devices(), Message::DiscoveryComplete)
            }

            Message::DiscoveryComplete(devices) => {
                self.is_discovering = false;
                self.discovered_devices = devices.clone();

                if devices.is_empty() {
                    self.user_message = Some(UserMessage::Info(
                        "No Ultimate devices found on network".to_string(),
                    ));
                } else {
                    self.user_message = Some(UserMessage::Info(format!(
                        "Found {} device(s)",
                        devices.len()
                    )));
                }
                Task::none()
            }

            Message::SelectDiscoveredDevice(device) => {
                self.host_input = device.ip.clone();
                self.user_message = Some(UserMessage::Info(format!(
                    "Selected: {} ({})",
                    device.product, device.ip
                )));
                Task::none()
            }
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
                    let host = Some(self.settings.connection.host.clone());
                    let pwd = self.settings.connection.password.clone();
                    let refresh = self
                        .assembly64_browser
                        .update(
                            Assembly64BrowserMessage::RefreshPresets,
                            self.connection.clone(),
                            host.clone(),
                            pwd.clone(),
                        )
                        .map(Message::Assembly64Browser);
                    let search = self
                        .assembly64_browser
                        .update(
                            Assembly64BrowserMessage::SearchSubmit,
                            self.connection.clone(),
                            host,
                            pwd,
                        )
                        .map(Message::Assembly64Browser);
                    return Task::batch([refresh, search]);
                }
                Task::none()
            }
            Message::CloseStreamingWindow => {
                if let Some(id) = self.streaming_window_id {
                    self.streaming_window_id = None;
                    return iced::window::close(id);
                }
                Task::none()
            }
            Message::MemoryEditor(msg) => self
                .memory_editor
                .update(
                    msg,
                    self.connection.clone(),
                    Some(self.settings.connection.host.clone()),
                    self.settings.connection.password.clone(),
                )
                .map(Message::MemoryEditor),
            Message::Monitor(msg) => self
                .sid_monitor
                .update(msg, self.connection.clone())
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
                    .update(
                        msg,
                        self.connection.clone(),
                        Some(self.settings.connection.host.clone()),
                        self.settings.connection.password.clone(),
                    )
                    .map(Message::LeftBrowser);

                let mut commands = vec![cmd];

                // Stop music player if running a cartridge/disk
                if should_stop_music {
                    log::info!("Stopping music player - running cartridge/disk");
                    commands.push(
                        self.music_player
                            .update(MusicPlayerMessage::Stop, self.connection.clone())
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
                        .update(msg, self.connection.clone())
                        .map(Message::RemoteBrowser);
                    let refresh = self
                        .left_browser
                        .update(
                            FileBrowserMessage::RefreshFiles,
                            self.connection.clone(),
                            Some(self.settings.connection.host.clone()),
                            self.settings.connection.password.clone(),
                        )
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
                    .update(msg, self.connection.clone())
                    .map(Message::RemoteBrowser);

                if should_stop_music {
                    log::info!("Stopping music player - running file from Ultimate64");
                    Task::batch(vec![
                        cmd,
                        self.music_player
                            .update(MusicPlayerMessage::Stop, self.connection.clone())
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
                    .update(
                        FileBrowserMessage::CalculateSelectedSize,
                        self.connection.clone(),
                        Some(self.settings.connection.host.clone()),
                        self.settings.connection.password.clone(),
                    )
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
                                .update(
                                    FileBrowserMessage::ShowContentPreview(path),
                                    self.connection.clone(),
                                    Some(self.settings.connection.host.clone()),
                                    self.settings.connection.password.clone(),
                                )
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
                                .update(
                                    RemoteBrowserMessage::ShowContentPreview(path),
                                    self.connection.clone(),
                                )
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
                    .update(
                        FileBrowserMessage::RefreshFiles,
                        self.connection.clone(),
                        Some(self.settings.connection.host.clone()),
                        self.settings.connection.password.clone(),
                    )
                    .map(Message::LeftBrowser),
                Pane::Right => self
                    .remote_browser
                    .update(RemoteBrowserMessage::RefreshFiles, self.connection.clone())
                    .map(Message::RemoteBrowser),
            },

            Message::FnMkDir => match self.active_pane {
                Pane::Left => self
                    .left_browser
                    .update(
                        FileBrowserMessage::ShowCreateDir,
                        self.connection.clone(),
                        Some(self.settings.connection.host.clone()),
                        self.settings.connection.password.clone(),
                    )
                    .map(Message::LeftBrowser),
                Pane::Right => self
                    .remote_browser
                    .update(RemoteBrowserMessage::ShowCreateDir, self.connection.clone())
                    .map(Message::RemoteBrowser),
            },

            Message::FnNewDisk => match self.active_pane {
                Pane::Left => self
                    .left_browser
                    .update(
                        FileBrowserMessage::ShowCreateDisk,
                        self.connection.clone(),
                        Some(self.settings.connection.host.clone()),
                        self.settings.connection.password.clone(),
                    )
                    .map(Message::LeftBrowser),
                Pane::Right => self
                    .remote_browser
                    .update(
                        RemoteBrowserMessage::ShowCreateDisk,
                        self.connection.clone(),
                    )
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
                                .update(
                                    FileBrowserMessage::DeleteChecked,
                                    self.connection.clone(),
                                    Some(self.settings.connection.host.clone()),
                                    self.settings.connection.password.clone(),
                                )
                                .map(Message::LeftBrowser);
                        }
                        Task::none()
                    }
                    Pane::Right => {
                        let checked_count = self.remote_browser.checked_files.len();
                        if checked_count > 0 {
                            return self
                                .remote_browser
                                .update(
                                    RemoteBrowserMessage::DeleteChecked,
                                    self.connection.clone(),
                                )
                                .map(Message::RemoteBrowser);
                        }
                        Task::none()
                    }
                }
            }

            Message::FilterChanged(text) => match self.active_pane {
                Pane::Left => self
                    .left_browser
                    .update(
                        FileBrowserMessage::FilterChanged(text),
                        self.connection.clone(),
                        Some(self.settings.connection.host.clone()),
                        self.settings.connection.password.clone(),
                    )
                    .map(Message::LeftBrowser),
                Pane::Right => self
                    .remote_browser
                    .update(
                        RemoteBrowserMessage::FilterChanged(text),
                        self.connection.clone(),
                    )
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
                    .update(
                        FileBrowserMessage::NavigateUp,
                        self.connection.clone(),
                        Some(self.settings.connection.host.clone()),
                        self.settings.connection.password.clone(),
                    )
                    .map(Message::LeftBrowser),
                Pane::Right => self
                    .remote_browser
                    .update(RemoteBrowserMessage::NavigateUp, self.connection.clone())
                    .map(Message::RemoteBrowser),
            },

            Message::SelectAllActivePane => match self.active_pane {
                Pane::Left => self
                    .left_browser
                    .update(
                        FileBrowserMessage::SelectAll,
                        self.connection.clone(),
                        Some(self.settings.connection.host.clone()),
                        self.settings.connection.password.clone(),
                    )
                    .map(Message::LeftBrowser),
                Pane::Right => self
                    .remote_browser
                    .update(RemoteBrowserMessage::SelectAll, self.connection.clone())
                    .map(Message::RemoteBrowser),
            },

            Message::SelectNoneActivePane => match self.active_pane {
                Pane::Left => self
                    .left_browser
                    .update(
                        FileBrowserMessage::SelectNone,
                        self.connection.clone(),
                        Some(self.settings.connection.host.clone()),
                        self.settings.connection.password.clone(),
                    )
                    .map(Message::LeftBrowser),
                Pane::Right => self
                    .remote_browser
                    .update(RemoteBrowserMessage::SelectNone, self.connection.clone())
                    .map(Message::RemoteBrowser),
            },

            Message::FnRename => {
                match self.active_pane {
                    Pane::Left => {
                        // Local rename not supported via function bar yet
                        self.user_message = Some(UserMessage::Info(
                            "Select a file and use your OS to rename local files".to_string(),
                        ));
                        Task::none()
                    }
                    Pane::Right => {
                        // Rename the selected file on remote
                        if let Some(ref selected) = self.remote_browser.selected_file {
                            let path = selected.clone();
                            return self
                                .remote_browser
                                .update(
                                    RemoteBrowserMessage::RenameFile(path),
                                    self.connection.clone(),
                                )
                                .map(Message::RemoteBrowser);
                        }
                        self.user_message = Some(UserMessage::Info(
                            "Click a file first, then press F2 to rename".to_string(),
                        ));
                        Task::none()
                    }
                }
            }

            Message::CopyLocalToRemote => {
                // Copy checked local files and directories to Ultimate64.
                // Before kicking off the FTP upload, check which top-level
                // destination names already exist on the device — if any do,
                // stash the operation in `pending_copy` and let the overwrite
                // dialog gate the actual transfer.
                let items_to_copy = self.left_browser.get_checked_files();

                if items_to_copy.is_empty() {
                    self.user_message = Some(UserMessage::Error(
                        "No files selected. Use checkboxes to select files.".to_string(),
                    ));
                    return Task::none();
                }

                if self.host_url.is_none() {
                    self.user_message = Some(UserMessage::Error(
                        "Not connected to Ultimate64".to_string(),
                    ));
                    return Task::none();
                }

                let remote_dest = self.remote_browser.get_current_path().to_string();
                let existing: std::collections::HashSet<&str> = self
                    .remote_browser
                    .files
                    .iter()
                    .map(|f| f.name.as_str())
                    .collect();
                let conflicts: Vec<String> = items_to_copy
                    .iter()
                    .filter_map(|p| p.file_name().and_then(|n| n.to_str()))
                    .filter(|n| existing.contains(n))
                    .map(String::from)
                    .collect();

                self.pending_copy = Some(PendingCopy {
                    items: items_to_copy,
                    remote_dest,
                    conflicts: conflicts.clone(),
                });
                if conflicts.is_empty() {
                    return Task::done(Message::CopyOverwriteConfirm);
                }
                Task::none()
            }
            Message::CopyOverwriteCancel => {
                self.pending_copy = None;
                Task::none()
            }
            Message::CopyOverwriteConfirm => {
                let pending = match self.pending_copy.take() {
                    Some(p) => p,
                    None => return Task::none(),
                };
                let items_to_copy = pending.items;
                let remote_dest = pending.remote_dest;

                let host = match &self.host_url {
                    Some(h) => h
                        .trim_start_matches("http://")
                        .trim_start_matches("https://")
                        .to_string(),
                    None => {
                        self.user_message = Some(UserMessage::Error(
                            "Not connected to Ultimate64".to_string(),
                        ));
                        return Task::none();
                    }
                };

                let password = self.settings.connection.password.clone();
                let progress = self.copy_progress.clone();

                // Count total files and bytes (recursively walking directories)
                let mut total_files: usize = 0;
                let mut total_bytes: u64 = 0;
                for p in &items_to_copy {
                    if p.is_dir() {
                        for e in walkdir::WalkDir::new(p)
                            .min_depth(1)
                            .into_iter()
                            .filter_map(|e| e.ok())
                        {
                            if e.file_type().is_file() {
                                total_files += 1;
                                total_bytes = total_bytes
                                    .saturating_add(e.metadata().map(|m| m.len()).unwrap_or(0));
                            }
                        }
                    } else {
                        total_files += 1;
                        total_bytes = total_bytes
                            .saturating_add(std::fs::metadata(p).map(|m| m.len()).unwrap_or(0));
                    }
                }

                if let Ok(mut g) = progress.lock() {
                    *g = Some(crate::ftp_ops::TransferProgress {
                        current: 0,
                        total: total_files,
                        current_file: String::new(),
                        operation: "Uploading".to_string(),
                        done: false,
                        cancelled: false,
                        started_at: std::time::Instant::now(),
                        bytes_transferred: 0,
                        bytes_total: total_bytes,
                    });
                }

                self.user_message = Some(UserMessage::Info(format!(
                    "Uploading {} file(s) via FTP...",
                    total_files
                )));

                return Task::perform(
                    async move {
                        tokio::task::spawn_blocking(move || {
                            use std::io::Cursor;
                            use std::time::Duration;
                            use suppaftp::FtpStream;

                            let addr = format!("{}:21", host);
                            let mut ftp = FtpStream::connect(&addr)
                                .map_err(|e| format!("FTP connect failed: {}", e))?;

                            ftp.get_ref()
                                .set_write_timeout(Some(Duration::from_secs(120)))
                                .ok();

                            if let Some(ref pwd) = password {
                                if !pwd.is_empty() {
                                    ftp.login("admin", pwd)
                                        .map_err(|e| format!("FTP login failed: {}", e))?;
                                } else {
                                    ftp.login("anonymous", "anonymous")
                                        .map_err(|e| format!("FTP login failed: {}", e))?;
                                }
                            } else {
                                ftp.login("anonymous", "anonymous")
                                    .map_err(|e| format!("FTP login failed: {}", e))?;
                            }

                            ftp.transfer_type(suppaftp::types::FileType::Binary)
                                .map_err(|e| format!("Set binary mode failed: {}", e))?;

                            let mut uploaded = 0usize;
                            let mut errors: Vec<String> = Vec::new();

                            for item_path in &items_to_copy {
                                // Check for cancellation
                                let is_cancelled = progress
                                    .lock()
                                    .ok()
                                    .and_then(|g| g.as_ref().map(|p| p.cancelled))
                                    .unwrap_or(false);
                                if is_cancelled {
                                    break;
                                }

                                if item_path.is_dir() {
                                    // Upload directory recursively
                                    let dir_name = item_path
                                        .file_name()
                                        .and_then(|n| n.to_str())
                                        .unwrap_or("dir");
                                    let base_remote = format!(
                                        "{}/{}",
                                        remote_dest.trim_end_matches('/'),
                                        dir_name
                                    );

                                    for entry in walkdir::WalkDir::new(item_path).min_depth(0) {
                                        // Check for cancellation inside dir walk
                                        let is_cancelled = progress
                                            .lock()
                                            .ok()
                                            .and_then(|g| g.as_ref().map(|p| p.cancelled))
                                            .unwrap_or(false);
                                        if is_cancelled {
                                            break;
                                        }
                                        let entry = match entry {
                                            Ok(e) => e,
                                            Err(e) => {
                                                errors.push(format!("Walk error: {}", e));
                                                continue;
                                            }
                                        };
                                        let relative = match entry.path().strip_prefix(item_path) {
                                            Ok(r) => r,
                                            Err(_) => continue,
                                        };
                                        let remote_path = if relative.as_os_str().is_empty() {
                                            base_remote.clone()
                                        } else {
                                            let rel_str =
                                                relative.to_string_lossy().replace('\\', "/");
                                            format!("{}/{}", base_remote, rel_str)
                                        };

                                        if entry.file_type().is_dir() {
                                            let _ = ftp.mkdir(&remote_path);
                                        } else if entry.file_type().is_file() {
                                            let filename = entry
                                                .path()
                                                .file_name()
                                                .unwrap_or_default()
                                                .to_string_lossy()
                                                .to_string();

                                            if let Ok(mut g) = progress.lock() {
                                                if let Some(ref mut p) = *g {
                                                    p.current_file = filename.clone();
                                                }
                                            }

                                            match std::fs::read(entry.path()) {
                                                Ok(data) => {
                                                    let (parent_dir, fname) =
                                                        if let Some(pos) = remote_path.rfind('/') {
                                                            (
                                                                &remote_path[..pos],
                                                                &remote_path[pos + 1..],
                                                            )
                                                        } else {
                                                            ("/", remote_path.as_str())
                                                        };
                                                    if ftp.cwd(parent_dir).is_err() {
                                                        errors.push(format!(
                                                            "CWD {}: failed",
                                                            parent_dir
                                                        ));
                                                        continue;
                                                    }
                                                    let cursor = Cursor::new(data);
                                                    let mut reader =
                                                        crate::ftp_ops::ProgressReader {
                                                            inner: cursor,
                                                            progress: progress.clone(),
                                                        };
                                                    match ftp.put_file(fname, &mut reader) {
                                                        Ok(_) => {
                                                            uploaded += 1;
                                                            if let Ok(mut g) = progress.lock() {
                                                                if let Some(ref mut p) = *g {
                                                                    p.current = uploaded;
                                                                }
                                                            }
                                                        }
                                                        Err(e) => errors.push(format!(
                                                            "Upload {}: {}",
                                                            fname, e
                                                        )),
                                                    }
                                                }
                                                Err(e) => errors.push(format!(
                                                    "Read {}: {}",
                                                    entry.path().display(),
                                                    e
                                                )),
                                            }
                                        }
                                    }
                                } else {
                                    // Upload single file
                                    let filename = item_path
                                        .file_name()
                                        .and_then(|n| n.to_str())
                                        .unwrap_or("file")
                                        .to_string();

                                    if let Ok(mut g) = progress.lock() {
                                        if let Some(ref mut p) = *g {
                                            p.current_file = filename.clone();
                                        }
                                    }

                                    // CWD to remote dest for loose files
                                    ftp.cwd(&remote_dest).map_err(|e| {
                                        format!("Cannot access {}: {}", remote_dest, e)
                                    })?;

                                    let data = std::fs::read(item_path).map_err(|e| {
                                        format!("Cannot read {}: {}", item_path.display(), e)
                                    })?;
                                    let cursor = Cursor::new(data);
                                    let mut reader = crate::ftp_ops::ProgressReader {
                                        inner: cursor,
                                        progress: progress.clone(),
                                    };
                                    ftp.put_file(&filename, &mut reader).map_err(|e| {
                                        format!("FTP upload {} failed: {}", filename, e)
                                    })?;

                                    uploaded += 1;
                                    if let Ok(mut g) = progress.lock() {
                                        if let Some(ref mut p) = *g {
                                            p.current = uploaded;
                                        }
                                    }
                                }
                            }

                            let was_cancelled = progress
                                .lock()
                                .ok()
                                .and_then(|g| g.as_ref().map(|p| p.cancelled))
                                .unwrap_or(false);

                            if let Ok(mut g) = progress.lock() {
                                if let Some(ref mut p) = *g {
                                    p.done = true;
                                }
                            }

                            let _ = ftp.quit();

                            let mut msg = if was_cancelled {
                                format!("Cancelled after {} file(s)", uploaded)
                            } else {
                                format!("Uploaded {} file(s)", uploaded)
                            };
                            if !errors.is_empty() {
                                msg.push_str(&format!(" ({} errors)", errors.len()));
                                for e in errors.iter().take(3) {
                                    log::warn!("Upload error: {}", e);
                                }
                            }
                            Ok(msg)
                        })
                        .await
                        .map_err(|e| e.to_string())?
                    },
                    Message::CopyComplete,
                );
            }
            Message::CopyRemoteToLocal => {
                let (file_paths, dir_paths) = self.remote_browser.get_checked_files_and_dirs();

                // Fall back to single selected file if nothing checked
                let (file_paths, dir_paths) = if file_paths.is_empty() && dir_paths.is_empty() {
                    if let Some(path) = self.remote_browser.get_selected_file() {
                        (vec![path.to_string()], vec![])
                    } else {
                        self.user_message = Some(UserMessage::Error(
                            "No files selected. Use checkboxes to select files.".to_string(),
                        ));
                        return Task::none();
                    }
                } else {
                    (file_paths, dir_paths)
                };

                let host = match &self.host_url {
                    Some(h) => h
                        .trim_start_matches("http://")
                        .trim_start_matches("https://")
                        .to_string(),
                    None => {
                        self.user_message = Some(UserMessage::Error(
                            "Not connected to Ultimate64".to_string(),
                        ));
                        return Task::none();
                    }
                };

                let local_dest = self.left_browser.get_current_directory().clone();
                let password = self.settings.connection.password.clone();
                let progress = self.copy_progress.clone();

                // Initial total is just file count; directories will be counted via FTP LIST
                if let Ok(mut g) = progress.lock() {
                    *g = Some(crate::ftp_ops::TransferProgress {
                        current: 0,
                        total: file_paths.len(),
                        current_file: "counting files...".to_string(),
                        operation: "Downloading".to_string(),
                        done: false,
                        cancelled: false,
                        started_at: std::time::Instant::now(),
                        bytes_transferred: 0,
                        bytes_total: 0,
                    });
                }

                self.user_message = Some(UserMessage::Info("Downloading via FTP...".to_string()));

                return Task::perform(
                    async move {
                        tokio::task::spawn_blocking(move || {
                            use std::io::Read;
                            use std::time::Duration;
                            use suppaftp::FtpStream;

                            let addr = format!("{}:21", host);
                            let mut ftp = FtpStream::connect(&addr)
                                .map_err(|e| format!("FTP connect failed: {}", e))?;

                            ftp.get_ref()
                                .set_read_timeout(Some(Duration::from_secs(60)))
                                .ok();

                            if let Some(ref pwd) = password {
                                if !pwd.is_empty() {
                                    ftp.login("admin", pwd)
                                        .map_err(|e| format!("FTP login failed: {}", e))?;
                                } else {
                                    ftp.login("anonymous", "anonymous")
                                        .map_err(|e| format!("FTP login failed: {}", e))?;
                                }
                            } else {
                                ftp.login("anonymous", "anonymous")
                                    .map_err(|e| format!("FTP login failed: {}", e))?;
                            }

                            ftp.transfer_type(suppaftp::types::FileType::Binary)
                                .map_err(|e| format!("Set binary mode failed: {}", e))?;

                            let mut downloaded = 0usize;
                            let mut errors: Vec<String> = Vec::new();

                            // Count total files in directories via FTP LIST
                            let mut dir_file_count = 0usize;
                            for remote_dir in &dir_paths {
                                dir_file_count +=
                                    count_remote_files_recursive(&mut ftp, remote_dir);
                            }
                            // Update total with actual file count
                            if let Ok(mut g) = progress.lock() {
                                if let Some(ref mut p) = *g {
                                    p.total = file_paths.len() + dir_file_count;
                                    p.current_file = String::new();
                                }
                            }

                            // Download individual files
                            for remote_path in &file_paths {
                                let is_cancelled = progress
                                    .lock()
                                    .ok()
                                    .and_then(|g| g.as_ref().map(|p| p.cancelled))
                                    .unwrap_or(false);
                                if is_cancelled {
                                    break;
                                }

                                let filename = remote_path.rsplit('/').next().unwrap_or("file");

                                // Get file size for progress
                                let file_size = ftp.size(remote_path).unwrap_or(0);

                                if let Ok(mut g) = progress.lock() {
                                    if let Some(ref mut p) = *g {
                                        p.current_file = filename.to_string();
                                        p.bytes_total += file_size as u64;
                                    }
                                }

                                match ftp.retr_as_stream(remote_path) {
                                    Ok(mut reader) => {
                                        let mut data = Vec::new();
                                        if let Err(e) = reader.read_to_end(&mut data) {
                                            errors.push(format!("{}: {}", filename, e));
                                            continue;
                                        }
                                        if let Err(e) = ftp.finalize_retr_stream(reader) {
                                            errors.push(format!("{}: {}", filename, e));
                                            continue;
                                        }
                                        let local_path = local_dest.join(filename);
                                        if let Err(e) = std::fs::write(&local_path, &data) {
                                            errors.push(format!("{}: {}", filename, e));
                                            continue;
                                        }
                                        downloaded += 1;
                                        if let Ok(mut g) = progress.lock() {
                                            if let Some(ref mut p) = *g {
                                                p.current = downloaded;
                                                p.bytes_transferred += data.len() as u64;
                                            }
                                        }
                                    }
                                    Err(e) => {
                                        errors.push(format!("{}: {}", filename, e));
                                    }
                                }
                            }

                            // Download directories recursively
                            for remote_dir in &dir_paths {
                                let is_cancelled = progress
                                    .lock()
                                    .ok()
                                    .and_then(|g| g.as_ref().map(|p| p.cancelled))
                                    .unwrap_or(false);
                                if is_cancelled {
                                    break;
                                }

                                let dir_name = remote_dir.rsplit('/').next().unwrap_or("dir");
                                let local_dir = local_dest.join(dir_name);

                                if let Ok(mut g) = progress.lock() {
                                    if let Some(ref mut p) = *g {
                                        p.current_file = format!("{}/", dir_name);
                                    }
                                }

                                match download_directory_with_progress(
                                    &mut ftp,
                                    remote_dir,
                                    &local_dir,
                                    &progress,
                                    &mut downloaded,
                                ) {
                                    Ok(files) => {
                                        log::info!("Downloaded dir {}: {} files", dir_name, files);
                                    }
                                    Err(e) => {
                                        errors.push(format!("{}: {}", dir_name, e));
                                    }
                                }
                            }

                            let was_cancelled = progress
                                .lock()
                                .ok()
                                .and_then(|g| g.as_ref().map(|p| p.cancelled))
                                .unwrap_or(false);

                            if let Ok(mut g) = progress.lock() {
                                if let Some(ref mut p) = *g {
                                    p.done = true;
                                }
                            }

                            let _ = ftp.quit();

                            let mut msg = if was_cancelled {
                                format!("Cancelled after {} item(s)", downloaded)
                            } else {
                                format!("Downloaded {} item(s)", downloaded)
                            };
                            if !errors.is_empty() {
                                msg.push_str(&format!(" ({} errors)", errors.len()));
                                for e in errors.iter().take(3) {
                                    log::warn!("Download error: {}", e);
                                }
                            }
                            Ok(msg)
                        })
                        .await
                        .map_err(|e| e.to_string())?
                    },
                    Message::CopyComplete,
                );
            }

            Message::CopyCancel => {
                if let Ok(mut g) = self.copy_progress.lock() {
                    if let Some(ref mut p) = *g {
                        p.cancelled = true;
                    }
                }
                Task::none()
            }
            Message::CopyProgressTick => {
                // Just triggers a re-render so the progress bar updates
                Task::none()
            }
            Message::CopyComplete(result) => {
                // Clear copy progress
                if let Ok(mut g) = self.copy_progress.lock() {
                    *g = None;
                }
                match result {
                    Ok(msg) => {
                        self.show_toast(msg.clone());
                        self.user_message = Some(UserMessage::Info(msg));
                        // Clear checked files after successful copy
                        self.left_browser.clear_checked();
                        // Refresh both browsers
                        return Task::batch(vec![
                            self.left_browser
                                .update(
                                    FileBrowserMessage::RefreshFiles,
                                    self.connection.clone(),
                                    Some(self.settings.connection.host.clone()),
                                    self.settings.connection.password.clone(),
                                )
                                .map(Message::LeftBrowser),
                            self.remote_browser
                                .update(RemoteBrowserMessage::RefreshFiles, self.connection.clone())
                                .map(Message::RemoteBrowser),
                        ]);
                    }
                    Err(e) => {
                        self.user_message = Some(UserMessage::Error(e));
                    }
                }
                Task::none()
            }
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
                        .update(msg, self.connection.clone())
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
                            .update(msg, self.connection.clone())
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
                    .update(msg, self.connection.clone())
                    .map(Message::MusicPlayer)
            }

            Message::ConfigEditor(msg) => self
                .config_editor
                .update(
                    msg,
                    self.connection.clone(),
                    self.host_url.clone(),
                    self.settings.connection.password.clone(),
                )
                .map(Message::ConfigEditor),

            Message::DeviceProfileManager(msg) => {
                // Provide streaming frame buffer for screenshot capture
                self.device_profile_manager
                    .set_streaming_frame(self.video_streaming.frame_buffer.clone());
                self.device_profile_manager
                    .update(
                        msg,
                        self.host_url.clone(),
                        self.settings.connection.password.clone(),
                        self.connection.clone(),
                    )
                    .map(Message::DeviceProfileManager)
            }

            Message::HostInputChanged(value) => {
                self.host_input = value;
                Task::none()
            }

            Message::PasswordInputChanged(value) => {
                self.password_input = value;
                Task::none()
            }

            Message::ConnectPressed => {
                log::info!("Connect button pressed");
                let conn_settings = ConnectionSettings {
                    host: self.host_input.clone(),
                    password: if self.password_input.is_empty() {
                        None
                    } else {
                        Some(self.password_input.clone())
                    },
                    stream_control_method: self.settings.connection.stream_control_method,
                };
                self.profile_manager.active_settings_mut().connection = conn_settings;
                self.settings = self.profile_manager.active_settings().clone();

                self.establish_connection();
                // Trigger status refresh and remote browser refresh after a short delay
                Task::perform(
                    async {
                        tokio::time::sleep(tokio::time::Duration::from_millis(500)).await;
                    },
                    |_| Message::RefreshAfterConnect,
                )
            }

            Message::DisconnectPressed => {
                log::info!("Disconnecting...");

                // Stop video streaming if active to prevent hangs
                if self.video_streaming.is_streaming {
                    let _ = self
                        .video_streaming
                        .update(StreamingMessage::StopStream, None);
                }

                self.connection = None;
                self.host_url = None;
                self.status.connected = false;
                self.status.device_info = None;
                self.status.mounted_disks.clear();
                self.consecutive_status_failures = 0;
                self.remote_browser.set_host(None, None);
                // Clear telnet host for video streaming control
                self.video_streaming.set_ultimate_host(None);
                self.user_message = Some(UserMessage::Info(
                    "Disconnected from Ultimate64".to_string(),
                ));
                Task::none()
            }
            Message::StreamControlMethodChanged(method) => {
                self.profile_manager
                    .active_settings_mut()
                    .connection
                    .stream_control_method = method;
                self.settings = self.profile_manager.active_settings().clone();
                self.video_streaming.set_stream_control_method(method);
                if let Err(e) = self.profile_manager.save() {
                    log::error!("Failed to save profiles: {}", e);
                }
                Task::none()
            }
            Message::RefreshStatus => {
                if let Some(conn) = &self.connection {
                    let conn = conn.clone();
                    Task::perform(
                        async move { fetch_status(conn).await },
                        Message::StatusUpdated,
                    )
                } else {
                    Task::none()
                }
            }
            Message::ProfileSelected(name) => {
                if self.profile_manager.switch_profile(&name) {
                    self.settings = self.profile_manager.active_settings().clone();
                    self.host_input = self.settings.connection.host.clone();
                    self.password_input = self
                        .settings
                        .connection
                        .password
                        .clone()
                        .unwrap_or_default();
                    self.font_size_input = self.settings.preferences.font_size.to_string();
                    self.video_streaming
                        .set_stream_control_method(self.settings.connection.stream_control_method);

                    self.user_message =
                        Some(UserMessage::Info(format!("Switched to profile: {}", name)));
                    // Disconnect when switching profiles
                    return Task::done(Message::DisconnectPressed);
                }
                Task::none()
            }

            Message::NewProfileNameChanged(name) => {
                self.new_profile_name = name;
                Task::none()
            }

            Message::CreateProfile => {
                let name = self.new_profile_name.trim().to_string();
                if name.is_empty() {
                    self.user_message = Some(UserMessage::Error(
                        "Profile name cannot be empty".to_string(),
                    ));
                } else if self.profile_manager.add_profile(name.clone()) {
                    self.new_profile_name.clear();

                    self.user_message =
                        Some(UserMessage::Info(format!("Created profile: {}", name)));
                } else {
                    self.user_message = Some(UserMessage::Error(
                        "Profile name already exists".to_string(),
                    ));
                }
                Task::none()
            }

            Message::DuplicateProfile => {
                let new_name = format!("{} (copy)", self.profile_manager.active_profile);
                if self.profile_manager.duplicate_profile(
                    &self.profile_manager.active_profile.clone(),
                    new_name.clone(),
                ) {
                    self.user_message =
                        Some(UserMessage::Info(format!("Duplicated to: {}", new_name)));
                }
                Task::none()
            }

            Message::DeleteProfile => {
                let name = self.profile_manager.active_profile.clone();
                if self.profile_manager.delete_profile(&name) {
                    self.settings = self.profile_manager.active_settings().clone();

                    self.user_message =
                        Some(UserMessage::Info(format!("Deleted profile: {}", name)));
                } else {
                    self.user_message = Some(UserMessage::Error(
                        "Cannot delete active or last profile".to_string(),
                    ));
                }
                Task::none()
            }

            Message::RenameProfileNameChanged(name) => {
                self.rename_profile_name = name;
                Task::none()
            }

            Message::RenameProfile => {
                let new_name = self.rename_profile_name.trim().to_string();
                let old_name = self.profile_manager.active_profile.clone();
                if new_name.is_empty() {
                    self.user_message = Some(UserMessage::Error(
                        "Profile name cannot be empty".to_string(),
                    ));
                } else if self
                    .profile_manager
                    .rename_profile(&old_name, new_name.clone())
                {
                    self.rename_profile_name.clear();

                    self.user_message =
                        Some(UserMessage::Info(format!("Renamed to: {}", new_name)));
                } else {
                    self.user_message = Some(UserMessage::Error(
                        "Profile name already exists".to_string(),
                    ));
                }
                Task::none()
            }
            Message::Assembly64Browser(msg) => self
                .assembly64_browser
                .update(
                    msg,
                    self.connection.clone(),
                    Some(self.settings.connection.host.clone()),
                    self.settings.connection.password.clone(),
                )
                .map(Message::Assembly64Browser),
            Message::BasicEditor(msg) => self
                .basic_editor
                .update(
                    msg,
                    Some(self.settings.connection.host.clone()),
                    self.settings.connection.password.clone(),
                )
                .map(Message::BasicEditor),
            Message::RefreshAfterConnect => {
                // Refresh both status and remote browser after connection
                let status_cmd = if let Some(conn) = &self.connection {
                    let conn = conn.clone();
                    Task::perform(
                        async move { fetch_status(conn).await },
                        Message::StatusUpdated,
                    )
                } else {
                    Task::none()
                };

                let browser_cmd = self
                    .remote_browser
                    .update(RemoteBrowserMessage::RefreshFiles, self.connection.clone())
                    .map(Message::RemoteBrowser);

                Task::batch(vec![status_cmd, browser_cmd])
            }

            Message::StatusUpdated(result) => {
                match result {
                    Ok(status) => {
                        log::debug!(
                            "Status: Connected={}, Device={:?}, Disks={}",
                            status.connected,
                            status.device_info,
                            status.mounted_disks.len()
                        );
                        // Recovered from a transient outage — log it so the
                        // user can correlate with reboot events.
                        if self.consecutive_status_failures > 0 {
                            log::info!(
                                "Status recovered after {} failed poll(s)",
                                self.consecutive_status_failures
                            );
                        }
                        self.consecutive_status_failures = 0;
                        // Show connected message when first connecting
                        if !self.status.connected && status.connected {
                            self.user_message = Some(UserMessage::Info(format!(
                                "Connected to {}",
                                self.settings.connection.host
                            )));
                        }
                        self.status = status;
                    }
                    Err(e) => {
                        if self.remote_browser.is_transferring() {
                            log::debug!("Ignoring status failure during active transfer");
                            return Task::none();
                        }
                        // Ignore status failures during profile operations — the
                        // device may be rebooting or applying config, which
                        // legitimately takes it offline for 15-30 seconds.
                        if self.device_profile_manager.is_loading {
                            log::debug!("Ignoring status failure during profile operation");
                            return Task::none();
                        }

                        self.consecutive_status_failures =
                            self.consecutive_status_failures.saturating_add(1);

                        // Tolerate brief outages (reboots typically settle in
                        // 2-3 seconds) — keep the UI showing "Connected" and
                        // let the subscription poll back at the faster cadence.
                        if self.consecutive_status_failures < MAX_TRANSIENT_STATUS_FAILURES {
                            log::debug!(
                                "Status poll failed ({} of {}): {}",
                                self.consecutive_status_failures,
                                MAX_TRANSIENT_STATUS_FAILURES,
                                e
                            );
                            return Task::none();
                        }

                        log::warn!(
                            "Status update failed {} times — marking disconnected: {}",
                            self.consecutive_status_failures,
                            e
                        );
                        self.status.connected = false;
                        self.status.device_info = None;
                        // Stop streaming only if it was running
                        if self.video_streaming.is_streaming {
                            let _ = self
                                .video_streaming
                                .update(StreamingMessage::StopStream, None);
                        }
                    }
                }
                Task::none()
            }

            Message::TemplateSelected(template) => {
                self.selected_template = Some(template);
                Task::none()
            }

            Message::ExecuteTemplate => {
                if let Some(template) = &self.selected_template {
                    if let Some(conn) = &self.connection {
                        let conn = conn.clone();
                        let commands = template.commands.clone();
                        return Task::perform(
                            async move { execute_template_commands(conn, commands).await },
                            |result| match result {
                                Ok(_) => Message::RefreshStatus,
                                Err(e) => Message::ShowError(e),
                            },
                        );
                    } else {
                        self.user_message = Some(UserMessage::Error("Not connected".to_string()));
                    }
                }
                Task::none()
            }

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

            Message::DismissMessage => {
                self.user_message = None;
                Task::none()
            }

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
                                            let conn = conn.blocking_lock();
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
                    .update(msg, self.connection.clone())
                    .map(Message::Streaming)
            }

            Message::EscPressed => {
                // Priority order: dismiss the most-modal thing first.
                // Help overlay → eject confirm → drop dialog → fullscreen → pane quick-search.
                if self.show_help {
                    self.show_help = false;
                    return Task::none();
                }
                if self.pending_close.is_some() {
                    self.pending_close = None;
                    return Task::none();
                }
                if self.pending_eject_confirm {
                    self.pending_eject_confirm = false;
                    return Task::none();
                }
                if self.pending_drop.is_some() {
                    self.pending_drop = None;
                    return Task::none();
                }
                if self.video_streaming.is_fullscreen {
                    return self.update(Message::ExitFullscreen);
                }
                // Final fallback: clear the active pane's quick-search buffer.
                self.dispatch_local_pane_message(FileBrowserMessage::QuickSearchClear)
            }

            Message::ExitFullscreen => {
                // Only exit fullscreen if currently in fullscreen mode
                if self.video_streaming.is_fullscreen {
                    self.video_streaming.is_fullscreen = false;
                    // Exit fullscreen on the appropriate window
                    if let Some(streaming_id) = self.streaming_window_id {
                        return window::set_mode(streaming_id, iced::window::Mode::Windowed)
                            .map(|_: ()| Message::RefreshStatus);
                    } else {
                        return iced::window::oldest()
                            .and_then(|id| iced::window::set_mode(id, iced::window::Mode::Windowed))
                            .map(|_: ()| Message::RefreshStatus);
                    }
                }
                Task::none()
            }

            Message::OpenStreamingWindow => {
                if self.streaming_window_id.is_some() {
                    // Window already open
                    return Task::none();
                }
                let settings = iced::window::Settings {
                    size: iced::Size::new(800.0, 600.0),
                    min_size: Some(iced::Size::new(400.0, 300.0)),
                    decorations: true,
                    ..Default::default()
                };
                // Destructure the tuple - open() returns (Id, Task<Id>)
                let (id, open_task) = iced::window::open(settings);
                self.streaming_window_id = Some(id);
                open_task.map(move |_| Message::StreamingWindowOpened(id))
            }

            Message::StreamingWindowOpened(id) => {
                log::info!("Streaming window opened: {:?}", id);
                // ID already stored in OpenStreamingWindow handler
                Task::none()
            }

            Message::WindowCloseRequested(id) => {
                // Streaming window — close it without prompting; only the
                // main window holds in-flight transfers that we'd hate to
                // lose.
                if self.streaming_window_id == Some(id) {
                    return iced::window::close(id);
                }
                if self.main_window_id == Some(id) && self.is_transfer_in_flight() {
                    self.pending_close = Some(id);
                    Task::none()
                } else {
                    iced::window::close(id)
                }
            }
            Message::ConfirmCloseWindow => {
                if let Some(id) = self.pending_close.take() {
                    iced::window::close(id)
                } else {
                    Task::none()
                }
            }
            Message::CancelCloseWindow => {
                self.pending_close = None;
                Task::none()
            }
            Message::WindowClosed(id) => {
                if self.streaming_window_id == Some(id) {
                    // Streaming window was closed
                    log::info!("Streaming window closed: {:?}", id);
                    self.streaming_window_id = None;
                    Task::none()
                } else if self.main_window_id == Some(id) {
                    // Main window was closed - clean up immediately and exit
                    log::info!("Main window closed: {:?}", id);

                    // Cancel any in-progress copy transfer and clear it
                    if let Ok(mut g) = self.copy_progress.lock() {
                        if let Some(ref mut p) = *g {
                            p.cancelled = true;
                            p.done = true;
                        }
                    }

                    // Cancel any remote browser transfer
                    self.remote_browser.cancel_transfer();

                    // Stop streaming if active
                    if self.video_streaming.is_streaming {
                        self.video_streaming
                            .stop_signal
                            .store(true, std::sync::atomic::Ordering::Relaxed);
                    }

                    // Disconnect immediately to prevent further status checks
                    self.connection = None;
                    self.host_url = None;
                    self.status.connected = false;

                    // Mark main window as gone so subscriptions stop
                    self.main_window_id = None;

                    // Close any remaining windows and exit
                    if let Some(streaming_id) = self.streaming_window_id {
                        self.streaming_window_id = None;
                        return Task::batch(vec![iced::window::close(streaming_id), iced::exit()]);
                    }
                    iced::exit()
                } else {
                    Task::none()
                }
            }
            // ── Drag-and-drop from the OS ───────────────────────────────
            Message::FileDropped(path) => {
                // If a drop is already pending or one is in flight, ignore
                // the new one — the user can re-drop after dismissing the
                // dialog. Avoids the dialog stacking on accidental drops.
                if self.pending_drop.is_none() && !self.drop_in_flight {
                    log::info!("File dropped: {}", path.display());
                    self.pending_drop = Some(path);
                }
                Task::none()
            }
            Message::DropCancel => {
                self.pending_drop = None;
                Task::none()
            }
            Message::DropAction(action) => {
                self.pending_drop = None;
                self.user_message = Some(UserMessage::Info(format!("{}…", action.status_label())));
                let host_url = format!("http://{}", self.settings.connection.host);
                let password = self.settings.connection.password.clone();
                let remote_path = self.remote_browser.current_path.clone();
                // Only network actions need the cancel handle + in-flight
                // flag — OpenInBasicEditor is local fs reads that finish
                // in milliseconds and don't deserve a Cancel button.
                let is_network = !matches!(action, DropAction::OpenInBasicEditor { .. });
                self.drop_in_flight = is_network;
                let task = match action {
                    DropAction::RunOnDevice { path, runner } => Task::perform(
                        async move {
                            let bytes = tokio::fs::read(&path)
                                .await
                                .map_err(|e| format!("Read failed: {}", e))?;
                            tokio::time::timeout(
                                std::time::Duration::from_secs(30),
                                api::upload_runner_async(
                                    &host_url,
                                    runner,
                                    bytes,
                                    password.as_deref(),
                                ),
                            )
                            .await
                            .map_err(|_| "Send timed out — device offline?".to_string())?
                            .map(|_| {
                                format!(
                                    "Sent {} via {}",
                                    path.file_name().and_then(|s| s.to_str()).unwrap_or("file"),
                                    runner
                                )
                            })
                        },
                        Message::DropCompleted,
                    ),
                    DropAction::MountDisk { path } => {
                        let drive = "a"; // dropped images mount on the active drive A
                        Task::perform(
                            async move {
                                tokio::time::timeout(
                                    std::time::Duration::from_secs(30),
                                    api::upload_mount_disk_async(
                                        &host_url,
                                        &path,
                                        drive,
                                        "readonly",
                                        password.as_deref(),
                                    ),
                                )
                                .await
                                .map_err(|_| "Mount timed out — device offline?".to_string())?
                                .map(|_| {
                                    format!(
                                        "Mounted {} on Drive A (RO)",
                                        path.file_name().and_then(|s| s.to_str()).unwrap_or("disk")
                                    )
                                })
                            },
                            Message::DropCompleted,
                        )
                    }
                    DropAction::OpenInBasicEditor { path } => {
                        // Local file load — no network. Switch to the BASIC
                        // tab and reuse the editor's existing OpenCompleted
                        // message so it gets the same treatment as Open .bas.
                        self.active_tab = Tab::BasicEditor;
                        Task::perform(
                            async move {
                                let text = tokio::fs::read_to_string(&path)
                                    .await
                                    .map_err(|e| format!("Read failed: {}", e))?;
                                Ok::<_, String>((path, text))
                            },
                            |result| {
                                Message::BasicEditor(BasicEditorMessage::OpenCompleted(result))
                            },
                        )
                    }
                    DropAction::UploadToRemote { path } => {
                        let progress = self.remote_browser.transfer_progress_handle();
                        // FTP connect needs a bare host, no scheme — the
                        // user-configured host might include `http://`.
                        let host = self
                            .settings
                            .connection
                            .host
                            .trim_start_matches("http://")
                            .trim_start_matches("https://")
                            .trim_end_matches('/')
                            .to_string();
                        // `upload_file_ftp` only treats `remote_dest` as a
                        // directory when it ends with `/`. The remote
                        // browser's `current_path` is `/SD` (no trailing
                        // slash) once the user navigates anywhere, so we
                        // append one explicitly to avoid an empty CWD.
                        let dest = if remote_path.ends_with('/') {
                            remote_path
                        } else {
                            format!("{}/", remote_path)
                        };
                        Task::perform(
                            async move {
                                ftp_ops::upload_file_ftp(
                                    host,
                                    path.clone(),
                                    dest,
                                    password,
                                    progress,
                                )
                                .await
                                .map(|_| {
                                    format!(
                                        "Uploaded {}",
                                        path.file_name().and_then(|s| s.to_str()).unwrap_or("file")
                                    )
                                })
                            },
                            Message::DropCompleted,
                        )
                    }
                };
                // Wrap the network task in `abortable()` so the Cancel
                // button can drop the future without waiting for the
                // timeout. Local-only tasks (OpenInBasicEditor) skip this
                // — they have nothing to cancel.
                if is_network {
                    let (task, handle) = task.abortable();
                    self.drop_handle = Some(handle);
                    task
                } else {
                    task
                }
            }
            Message::DropCompleted(result) => {
                self.drop_in_flight = false;
                self.drop_handle = None;
                match &result {
                    Ok(msg) => self.show_toast(msg.clone()),
                    Err(_) => {}
                }
                self.user_message = Some(match result {
                    Ok(msg) => UserMessage::Info(msg),
                    Err(e) => UserMessage::Error(e),
                });
                Task::none()
            }
            Message::DropAbort => {
                if let Some(h) = self.drop_handle.take() {
                    h.abort();
                }
                self.drop_in_flight = false;
                self.user_message = Some(UserMessage::Info("Drop cancelled".into()));
                Task::none()
            }
            Message::ShowHelp => {
                self.show_help = true;
                Task::none()
            }
            Message::HideHelp => {
                self.show_help = false;
                Task::none()
            }

            Message::Nop => Task::none(),

            Message::ToastTick => {
                if let Some((_, shown_at)) = &self.toast {
                    if shown_at.elapsed() >= std::time::Duration::from_secs(TOAST_DURATION_SECS) {
                        self.toast = None;
                    }
                }
                Task::none()
            }

            Message::EjectAllDrives => {
                // Click on the toolbar button arms the confirmation modal —
                // the actual ejection is gated on EjectAllDrivesConfirmed
                // so an accidental click can't clear a hand-set mount.
                self.pending_eject_confirm = true;
                Task::none()
            }
            Message::EjectCancel => {
                self.pending_eject_confirm = false;
                Task::none()
            }
            Message::EjectAllDrivesConfirmed => {
                self.pending_eject_confirm = false;
                let host_url = format!("http://{}", self.settings.connection.host);
                let password = self.settings.connection.password.clone();
                // Fire both unmount calls in parallel; the device handles
                // them independently. Each has its own 5s REST timeout
                // baked into the client, so the whole op is bounded.
                let host_a = host_url.clone();
                let pwd_a = password.clone();
                let host_b = host_url;
                let pwd_b = password;
                Task::perform(
                    async move {
                        let (res_a, res_b) = tokio::join!(
                            api::unmount_disk_async(&host_a, "a", pwd_a.as_deref()),
                            api::unmount_disk_async(&host_b, "b", pwd_b.as_deref()),
                        );
                        match (res_a, res_b) {
                            (Ok(()), Ok(())) => Ok("Ejected Drives A and B".to_string()),
                            (Ok(()), Err(e)) => Err(format!("Drive A OK, Drive B failed: {}", e)),
                            (Err(e), Ok(())) => Err(format!("Drive B OK, Drive A failed: {}", e)),
                            (Err(a), Err(b)) => Err(format!("Both drives failed: {}; {}", a, b)),
                        }
                    },
                    Message::EjectCompleted,
                )
            }
            Message::EjectCompleted(result) => {
                if let Ok(msg) = &result {
                    self.show_toast(msg.clone());
                }
                self.user_message = Some(match result {
                    Ok(msg) => UserMessage::Info(msg),
                    Err(e) => UserMessage::Error(e),
                });
                Task::none()
            }
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
                    .update(
                        msg,
                        self.connection.clone(),
                        Some(self.settings.connection.host.clone()),
                        self.settings.connection.password.clone(),
                    )
                    .map(Message::LeftBrowser)
            }

            Message::ResetMachine => {
                if let Some(conn) = &self.connection {
                    let conn = conn.clone();
                    Task::perform(
                        async move {
                            let result = tokio::time::timeout(
                                tokio::time::Duration::from_secs(REST_TIMEOUT_SECS),
                                tokio::task::spawn_blocking(move || {
                                    let conn = conn.blocking_lock();
                                    conn.reset()
                                        .map(|_| "Machine reset successfully".to_string())
                                        .map_err(|e| format!("Reset failed: {}", e))
                                }),
                            )
                            .await;

                            match result {
                                Ok(Ok(r)) => r,
                                Ok(Err(e)) => Err(format!("Task error: {}", e)),
                                Err(_) => {
                                    Err("Reset timed out - device may be offline".to_string())
                                }
                            }
                        },
                        Message::MachineCommandCompleted,
                    )
                } else {
                    self.user_message = Some(UserMessage::Error("Not connected".to_string()));
                    Task::none()
                }
            }

            Message::RebootMachine => {
                if let Some(host) = &self.host_url {
                    let url = format!("{}/v1/machine:reboot", host);
                    Task::perform(
                        async move {
                            let client = crate::net_utils::build_device_client(REST_TIMEOUT_SECS)?;
                            client
                                .put(&url)
                                .send()
                                .await
                                .map_err(|e| format!("Reboot failed: {}", e))?;
                            Ok("Machine rebooting...".to_string())
                        },
                        Message::MachineCommandCompleted,
                    )
                } else {
                    self.user_message = Some(UserMessage::Error("Not connected".to_string()));
                    Task::none()
                }
            }

            Message::PauseMachine => {
                if let Some(host) = &self.host_url {
                    let url = format!("{}/v1/machine:pause", host);
                    Task::perform(
                        async move {
                            let client = crate::net_utils::build_device_client(REST_TIMEOUT_SECS)?;
                            client
                                .put(&url)
                                .send()
                                .await
                                .map_err(|e| format!("Pause failed: {}", e))?;
                            Ok("Machine paused".to_string())
                        },
                        Message::MachineCommandCompleted,
                    )
                } else {
                    self.user_message = Some(UserMessage::Error("Not connected".to_string()));
                    Task::none()
                }
            }

            Message::ResumeMachine => {
                if let Some(host) = &self.host_url {
                    let url = format!("{}/v1/machine:resume", host);
                    Task::perform(
                        async move {
                            let client = crate::net_utils::build_device_client(REST_TIMEOUT_SECS)?;
                            client
                                .put(&url)
                                .send()
                                .await
                                .map_err(|e| format!("Resume failed: {}", e))?;
                            Ok("Machine resumed".to_string())
                        },
                        Message::MachineCommandCompleted,
                    )
                } else {
                    self.user_message = Some(UserMessage::Error("Not connected".to_string()));
                    Task::none()
                }
            }

            Message::PoweroffMachine => {
                if let Some(conn) = &self.connection {
                    let conn = conn.clone();
                    Task::perform(
                        async move {
                            let result = tokio::time::timeout(
                                tokio::time::Duration::from_secs(REST_TIMEOUT_SECS),
                                tokio::task::spawn_blocking(move || {
                                    let conn = conn.blocking_lock();
                                    conn.poweroff()
                                        .map(|_| "Machine powered off".to_string())
                                        .map_err(|e| format!("Poweroff failed: {}", e))
                                }),
                            )
                            .await;

                            match result {
                                Ok(Ok(r)) => r,
                                Ok(Err(e)) => Err(format!("Task error: {}", e)),
                                Err(_) => {
                                    Err("Poweroff timed out - device may be offline".to_string())
                                }
                            }
                        },
                        Message::MachineCommandCompleted,
                    )
                } else {
                    self.user_message = Some(UserMessage::Error("Not connected".to_string()));
                    Task::none()
                }
            }

            Message::MenuButton => {
                if let Some(conn) = &self.connection {
                    let conn = conn.clone();
                    Task::perform(
                        async move {
                            let result = tokio::time::timeout(
                                tokio::time::Duration::from_secs(REST_TIMEOUT_SECS),
                                tokio::task::spawn_blocking(move || {
                                    let conn = conn.blocking_lock();
                                    conn.menu()
                                        .map(|_| "Menu button pressed".to_string())
                                        .map_err(|e| format!("Menu failed: {}", e))
                                }),
                            )
                            .await;

                            match result {
                                Ok(Ok(r)) => r,
                                Ok(Err(e)) => Err(format!("Task error: {}", e)),
                                Err(_) => Err("Menu timed out - device may be offline".to_string()),
                            }
                        },
                        Message::MachineCommandCompleted,
                    )
                } else {
                    self.user_message = Some(UserMessage::Error("Not connected".to_string()));
                    Task::none()
                }
            }

            Message::MachineCommandCompleted(result) => {
                match result {
                    Ok(msg) => {
                        self.user_message = Some(UserMessage::Info(msg));
                    }
                    Err(e) => {
                        self.user_message = Some(UserMessage::Error(e));
                    }
                }
                Task::none()
            }
            Message::DefaultSongDurationChanged(value) => {
                if let Ok(duration) = value.parse::<u32>() {
                    if duration > 0 && duration <= 3600 {
                        self.profile_manager
                            .active_settings_mut()
                            .preferences
                            .default_song_duration = duration;
                        self.settings = self.profile_manager.active_settings().clone();
                        self.music_player.set_default_song_duration(duration);
                        if let Err(e) = self.profile_manager.save() {
                            log::error!("Failed to save profiles: {}", e);
                        }
                    }
                }
                Task::none()
            }

            Message::FontSizeChanged(value) => {
                self.font_size_input = value.clone();
                if let Ok(size) = value.parse::<u32>() {
                    if size >= 8 && size <= 24 {
                        self.profile_manager
                            .active_settings_mut()
                            .preferences
                            .font_size = size;
                        self.settings = self.profile_manager.active_settings().clone();
                        if let Err(e) = self.profile_manager.save() {
                            log::error!("Failed to save profiles: {}", e);
                        }
                    }
                }
                Task::none()
            }
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
                if let Some(p) = path {
                    self.profile_manager
                        .active_settings_mut()
                        .default_paths
                        .file_browser_start_dir = Some(p);
                    self.settings = self.profile_manager.active_settings().clone();

                    self.user_message = Some(UserMessage::Info(
                        "File Browser start directory set (restart app to apply)".to_string(),
                    ));
                }
                Task::none()
            }

            Message::ClearFileBrowserStartDir => {
                self.profile_manager
                    .active_settings_mut()
                    .default_paths
                    .file_browser_start_dir = None;
                self.settings = self.profile_manager.active_settings().clone();
                if let Err(e) = self.profile_manager.save() {
                    log::error!("Failed to save profiles: {}", e);
                }
                self.user_message = Some(UserMessage::Info(
                    "File Browser start directory cleared".to_string(),
                ));
                Task::none()
            }

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
                if let Some(p) = path {
                    self.profile_manager
                        .active_settings_mut()
                        .default_paths
                        .music_player_start_dir = Some(p);
                    self.settings = self.profile_manager.active_settings().clone();

                    self.user_message = Some(UserMessage::Info(
                        "Music Player start directory set (restart app to apply)".to_string(),
                    ));
                }
                Task::none()
            }

            Message::ClearMusicPlayerStartDir => {
                self.profile_manager
                    .active_settings_mut()
                    .default_paths
                    .music_player_start_dir = None;
                self.settings = self.profile_manager.active_settings().clone();
                if let Err(e) = self.profile_manager.save() {
                    log::error!("Failed to save profiles: {}", e);
                }
                self.user_message = Some(UserMessage::Info(
                    "Music Player start directory cleared".to_string(),
                ));
                Task::none()
            }
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
        match self.active_pane {
            Pane::Left => self
                .left_browser
                .update(
                    msg,
                    self.connection.clone(),
                    Some(self.settings.connection.host.clone()),
                    self.settings.connection.password.clone(),
                )
                .map(Message::LeftBrowser),
            Pane::Right => Task::none(),
        }
    }

    /// Centered modal cheatsheet of every app-level keybind. Triggered by
    /// `?`; dismissed by Esc or the Close button. Edit `HELP_BINDS` to add
    /// new entries — keeping them in one table avoids documentation rot.
    fn view_help_overlay(&self) -> Element<'_, Message> {
        let fs = crate::styles::FontSizes::from_base(self.settings.preferences.font_size);

        let header = text("Keyboard Shortcuts").size(fs.large);
        let mut body = column![header, Space::new().height(10)].spacing(0);

        let mut current_section: Option<&'static str> = None;
        for (section, key, desc) in HELP_BINDS {
            if Some(*section) != current_section {
                if current_section.is_some() {
                    body = body.push(Space::new().height(8));
                }
                body = body.push(
                    text(*section)
                        .size(fs.normal)
                        .color(iced::Color::from_rgb(0.45, 0.65, 1.00)),
                );
                body = body.push(Space::new().height(4));
                current_section = Some(section);
            }
            body = body.push(
                row![
                    container(
                        text(*key)
                            .size(fs.small)
                            .color(iced::Color::from_rgb(0.85, 0.75, 0.45))
                    )
                    .width(Length::Fixed(180.0)),
                    text(*desc)
                        .size(fs.small)
                        .color(iced::Color::from_rgb(0.85, 0.85, 0.9)),
                ]
                .spacing(8),
            );
        }

        body = body.push(Space::new().height(14));
        body = body.push(
            row![
                Space::new().width(Length::Fill),
                button(text("Close").size(fs.small))
                    .on_press(Message::HideHelp)
                    .padding([6, 14]),
                Space::new().width(Length::Fill),
            ]
            .align_y(iced::Alignment::Center),
        );
        body = body.push(
            text("Press Esc to close")
                .size(fs.tiny)
                .color(iced::Color::from_rgb(0.55, 0.55, 0.6)),
        );

        let dialog = container(body.padding(20))
            .style(|_theme| container::Style {
                background: Some(iced::Background::Color(iced::Color::from_rgba(
                    0.18, 0.20, 0.28, 0.98,
                ))),
                border: iced::Border {
                    color: iced::Color::from_rgba(0.45, 0.52, 0.85, 0.7),
                    width: 1.0,
                    radius: 6.0.into(),
                },
                ..Default::default()
            })
            .width(Length::Fixed(520.0));

        container(dialog)
            .width(Length::Fill)
            .height(Length::Fill)
            .center_x(Length::Fill)
            .center_y(Length::Fill)
            .padding(20)
            .into()
    }

    fn view_drop_dialog(&self, path: &PathBuf) -> Element<'_, Message> {
        let fs = crate::styles::FontSizes::from_base(self.settings.preferences.font_size);
        let basename = path
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or("file")
            .to_string();
        let size_label = std::fs::metadata(path)
            .ok()
            .map(|m| crate::file_types::format_file_size(m.len()))
            .unwrap_or_else(|| "?".to_string());
        let ext = path
            .extension()
            .and_then(|s| s.to_str())
            .map(|s| s.to_lowercase())
            .unwrap_or_default();

        let header = text(format!("Dropped: {}  ({})", basename, size_label)).size(fs.normal);
        let path_line = text(path.display().to_string())
            .size(fs.tiny)
            .color(iced::Color::from_rgb(0.55, 0.55, 0.6));

        // Per-extension actions (may be empty for unknown types).
        let mut button_col = column![].spacing(8);
        for action in DropAction::available_for(&ext, path) {
            let label = action.button_label();
            button_col = button_col.push(
                button(text(label).size(fs.normal))
                    .on_press(Message::DropAction(action))
                    .padding([6, 14])
                    .width(Length::Fill),
            );
        }
        // Upload to remote — always available.
        button_col = button_col.push(
            button(
                text(DropAction::UploadToRemote { path: path.clone() }.button_label())
                    .size(fs.normal),
            )
            .on_press(Message::DropAction(DropAction::UploadToRemote {
                path: path.clone(),
            }))
            .padding([6, 14])
            .width(Length::Fill),
        );
        button_col = button_col.push(
            button(text("✕ Cancel").size(fs.normal))
                .on_press(Message::DropCancel)
                .padding([6, 14])
                .width(Length::Fill)
                .style(iced::widget::button::text),
        );

        let dialog = container(
            column![header, path_line, Space::new().height(8), button_col]
                .spacing(6)
                .padding(20),
        )
        .style(|_theme| container::Style {
            background: Some(iced::Background::Color(iced::Color::from_rgba(
                0.18, 0.20, 0.28, 0.98,
            ))),
            border: iced::Border {
                color: iced::Color::from_rgba(0.45, 0.52, 0.85, 0.7),
                width: 1.0,
                radius: 6.0.into(),
            },
            ..Default::default()
        })
        .width(Length::Fixed(420.0));

        // Click-outside-to-dismiss: the outer mouse_area covers the full
        // backdrop and fires DropCancel; the inner mouse_area around the
        // dialog absorbs clicks (with a Nop) so clicks ON the dialog itself
        // — between buttons, on the border, etc. — don't bubble through.
        // Button widgets capture their own clicks already, so this only
        // affects "dead" space inside the dialog.
        iced::widget::mouse_area(
            container(iced::widget::mouse_area(dialog).on_press(Message::Nop))
                .width(Length::Fill)
                .height(Length::Fill)
                .center_x(Length::Fill)
                .center_y(Length::Fill)
                .padding(20),
        )
        .on_press(Message::DropCancel)
        .into()
    }

    /// Confirmation modal for the Eject A+B toolbar button. Mirrors the
    /// drop-dialog overlay pattern so click-outside / Esc dismiss for free.
    fn view_eject_confirm_dialog(&self) -> Element<'_, Message> {
        let fs = crate::styles::FontSizes::from_base(self.settings.preferences.font_size);

        let dialog = container(
            column![
                text("Eject both drives?").size(fs.large),
                text("Drive A and Drive B will be cleared on the device. Any in-progress writes finish first; this cannot be undone.")
                    .size(fs.small)
                    .color(iced::Color::from_rgb(0.7, 0.7, 0.75)),
                Space::new().height(12),
                column![
                    button(text("⏏ Yes, eject A+B").size(fs.normal))
                        .on_press(Message::EjectAllDrivesConfirmed)
                        .padding([6, 14])
                        .width(Length::Fill)
                        .style(iced::widget::button::danger),
                    button(text("Cancel").size(fs.normal))
                        .on_press(Message::EjectCancel)
                        .padding([6, 14])
                        .width(Length::Fill)
                        .style(iced::widget::button::text),
                ]
                .spacing(8),
            ]
            .spacing(6)
            .padding(20),
        )
        .style(|_theme| container::Style {
            background: Some(iced::Background::Color(iced::Color::from_rgba(
                0.18, 0.20, 0.28, 0.98,
            ))),
            border: iced::Border {
                color: iced::Color::from_rgba(0.85, 0.4, 0.4, 0.7),
                width: 1.0,
                radius: 6.0.into(),
            },
            ..Default::default()
        })
        .width(Length::Fixed(380.0));

        iced::widget::mouse_area(
            container(iced::widget::mouse_area(dialog).on_press(Message::Nop))
                .width(Length::Fill)
                .height(Length::Fill)
                .center_x(Length::Fill)
                .center_y(Length::Fill)
                .padding(20),
        )
        .on_press(Message::EjectCancel)
        .into()
    }

    /// "Really close?" prompt shown when the user tried to quit while a
    /// transfer was in flight. Same overlay pattern as the other modals so
    /// click-outside cancels.
    fn view_close_confirm_dialog(&self) -> Element<'_, Message> {
        let fs = crate::styles::FontSizes::from_base(self.settings.preferences.font_size);

        let dialog = container(
            column![
                text("Quit while transfer is in progress?").size(fs.large),
                text("A file transfer or download hasn't finished yet. Closing now will abort it; partial files may be left behind.")
                    .size(fs.small)
                    .color(iced::Color::from_rgb(0.7, 0.7, 0.75)),
                Space::new().height(12),
                column![
                    button(text("Quit anyway").size(fs.normal))
                        .on_press(Message::ConfirmCloseWindow)
                        .padding([6, 14])
                        .width(Length::Fill)
                        .style(iced::widget::button::danger),
                    button(text("Keep working").size(fs.normal))
                        .on_press(Message::CancelCloseWindow)
                        .padding([6, 14])
                        .width(Length::Fill)
                        .style(iced::widget::button::text),
                ]
                .spacing(8),
            ]
            .spacing(6)
            .padding(20),
        )
        .style(|_theme| container::Style {
            background: Some(iced::Background::Color(iced::Color::from_rgba(
                0.18, 0.20, 0.28, 0.98,
            ))),
            border: iced::Border {
                color: iced::Color::from_rgba(0.85, 0.4, 0.4, 0.7),
                width: 1.0,
                radius: 6.0.into(),
            },
            ..Default::default()
        })
        .width(Length::Fixed(420.0));

        iced::widget::mouse_area(
            container(iced::widget::mouse_area(dialog).on_press(Message::Nop))
                .width(Length::Fill)
                .height(Length::Fill)
                .center_x(Length::Fill)
                .center_y(Length::Fill)
                .padding(20),
        )
        .on_press(Message::CancelCloseWindow)
        .into()
    }

    fn view_overwrite_dialog(&self, pending: &PendingCopy) -> Element<'_, Message> {
        let fs = crate::styles::FontSizes::from_base(self.settings.preferences.font_size);
        let n = pending.conflicts.len();
        let header = if n == 1 {
            format!("Overwrite 1 file in {}?", pending.remote_dest)
        } else {
            format!("Overwrite {} files in {}?", n, pending.remote_dest)
        };

        let mut list_col = column![].spacing(2);
        for name in pending.conflicts.iter().take(8) {
            list_col = list_col.push(
                text(format!("  • {}", name))
                    .size(fs.small)
                    .color(iced::Color::from_rgb(0.7, 0.7, 0.75)),
            );
        }
        if n > 8 {
            list_col = list_col.push(
                text(format!("  … and {} more", n - 8))
                    .size(fs.small)
                    .color(iced::Color::from_rgb(0.6, 0.6, 0.6)),
            );
        }

        container(
            column![
                text("⚠ Overwrite existing files")
                    .size(fs.large)
                    .color(iced::Color::from_rgb(1.0, 0.6, 0.3)),
                Space::new().height(8),
                text(header).size(fs.normal),
                Space::new().height(6),
                list_col,
                Space::new().height(12),
                text("Existing files with the same name will be replaced.")
                    .size(fs.small)
                    .color(iced::Color::from_rgb(0.9, 0.5, 0.5)),
                Space::new().height(16),
                row![
                    button(text("Cancel").size(fs.normal))
                        .on_press(Message::CopyOverwriteCancel)
                        .padding([6, 20])
                        .style(button::secondary),
                    Space::new().width(12),
                    button(text("Overwrite").size(fs.normal))
                        .on_press(Message::CopyOverwriteConfirm)
                        .padding([6, 20])
                        .style(button::danger),
                ]
                .align_y(iced::Alignment::Center),
            ]
            .align_x(iced::Alignment::Center)
            .spacing(2),
        )
        .padding(40)
        .center_x(Length::Fill)
        .center_y(Length::Fill)
        .into()
    }

    fn view_connection_bar(&self) -> Element<'_, Message> {
        let fs = crate::styles::FontSizes::from_base(self.settings.preferences.font_size);
        let status_indicator = if self.status.connected {
            text("● CONNECTED").color(iced::Color::from_rgb(0.2, 0.8, 0.2))
        } else {
            text("○ DISCONNECTED").color(iced::Color::from_rgb(0.8, 0.2, 0.2))
        };

        let device_text =
            text(self.status.device_info.as_deref().unwrap_or("No device")).size(fs.normal);

        // Update notification on the right side
        let update_notification: Element<'_, Message> = if let Some(info) = &self.new_version {
            row![
                text(format!("🎉 {} available!", info.version))
                    .size(fs.normal)
                    .color(iced::Color::from_rgb(0.3, 0.8, 0.3)),
                button(text("Download").size(fs.small))
                    .on_press(Message::OpenReleasePage)
                    .padding([2, 8])
                    .style(button::primary),
            ]
            .spacing(8)
            .align_y(iced::Alignment::Center)
            .into()
        } else {
            Space::new().into()
        };

        container(
            row![
                status_indicator,
                text(" | ").size(fs.normal),
                device_text,
                Space::new().width(Length::Fill),
                update_notification,
            ]
            .spacing(10)
            .align_y(iced::Alignment::Center),
        )
        .padding([8, 15])
        .into()
    }

    fn view_dual_pane_browser(&self) -> Element<'_, Message> {
        // Left pane - Local files
        let left_content = container(
            self.left_browser
                .view(self.settings.preferences.font_size)
                .map(Message::LeftBrowser),
        )
        .width(Length::FillPortion(1))
        .height(Length::Fill)
        .padding(2)
        .style(if self.active_pane == Pane::Left {
            crate::styles::active_pane_style
        } else {
            crate::styles::inactive_pane_style
        });

        let left_pane =
            iced::widget::mouse_area(left_content).on_press(Message::ActivePaneChanged(Pane::Left));

        // Right pane - Ultimate64 files
        let right_content = container(
            self.remote_browser
                .view(self.settings.preferences.font_size)
                .map(Message::RemoteBrowser),
        )
        .width(Length::FillPortion(1))
        .height(Length::Fill)
        .padding(2)
        .style(if self.active_pane == Pane::Right {
            crate::styles::active_pane_style
        } else {
            crate::styles::inactive_pane_style
        });

        let right_pane = iced::widget::mouse_area(right_content)
            .on_press(Message::ActivePaneChanged(Pane::Right));

        // Function bar at bottom
        let fs = crate::styles::FontSizes::from_base(self.settings.preferences.font_size);
        let small = fs.small as f32;
        let tiny = fs.tiny as f32;

        let active_filter = match self.active_pane {
            Pane::Left => self.left_browser.filter(),
            Pane::Right => self.remote_browser.filter(),
        };

        let copy_label = match self.active_pane {
            Pane::Left => "F5 Copy \u{2192}",
            Pane::Right => "F5 Copy \u{2190}",
        };

        let function_bar = container(
            row![
                button(text("F2 Ren").size(small))
                    .on_press(Message::FnRename)
                    .padding([4, 8])
                    .style(crate::styles::nav_button),
                button(text("F3 View").size(small))
                    .on_press(Message::FnView)
                    .padding([4, 8])
                    .style(crate::styles::nav_button),
                button(text("F4 Edit").size(small))
                    .on_press(Message::FnEdit)
                    .padding([4, 8])
                    .style(crate::styles::nav_button),
                button(text(copy_label).size(small))
                    .on_press(Message::FnCopy)
                    .padding([4, 8])
                    .style(crate::styles::nav_button),
                button(text("F7 MkDir").size(small))
                    .on_press(Message::FnMkDir)
                    .padding([4, 8])
                    .style(crate::styles::nav_button),
                button(text("New Disk").size(small))
                    .on_press(Message::FnNewDisk)
                    .padding([4, 8])
                    .style(crate::styles::nav_button),
                button(text("F8 Del").size(small))
                    .on_press(Message::FnDelete)
                    .padding([4, 8])
                    .style(crate::styles::nav_button),
                button(text("↻ Refresh").size(small))
                    .on_press(Message::FnRefresh)
                    .padding([4, 8])
                    .style(crate::styles::nav_button),
                // Device-control quick actions — gated on connection so an
                // offline click can't fire a hopeless REST request.
                button(text("⏏ Eject A+B").size(small))
                    .on_press_maybe(self.status.connected.then_some(Message::EjectAllDrives),)
                    .padding([4, 8])
                    .style(crate::styles::nav_button),
                // Run last — re-fires the most recent PRG/CRT/SID/disk
                // the local browser sent. Greys out when nothing's been
                // run yet OR when the device is offline.
                tooltip(
                    button(
                        text(match self.left_browser.last_run() {
                            Some(last) => format!("↪ Run last ({})", last.basename()),
                            None => "↪ Run last".to_string(),
                        })
                        .size(small),
                    )
                    .on_press_maybe(
                        (self.status.connected && self.left_browser.last_run().is_some())
                            .then_some(Message::RunLast),
                    )
                    .padding([4, 8])
                    .style(crate::styles::nav_button),
                    text(match self.left_browser.last_run() {
                        Some(last) => format!("Re-run {}", last.path().display()),
                        None => "Nothing has been run yet".to_string(),
                    })
                    .size(tiny),
                    tooltip::Position::Top,
                )
                .style(crate::styles::subtle_tooltip),
                text("|")
                    .size(tiny)
                    .color(iced::Color::from_rgb(0.4, 0.4, 0.45)),
                button(text("Sel All").size(small))
                    .on_press(Message::SelectAllActivePane)
                    .padding([4, 8])
                    .style(crate::styles::nav_button),
                button(text("Sel None").size(small))
                    .on_press(Message::SelectNoneActivePane)
                    .padding([4, 8])
                    .style(crate::styles::nav_button),
                Space::new().width(Length::Fill),
                text("Filter:")
                    .size(tiny)
                    .color(iced::Color::from_rgb(0.6, 0.6, 0.65)),
                text_input("filter...", active_filter)
                    .on_input(Message::FilterChanged)
                    .size(small)
                    .padding(4)
                    .width(Length::Fixed(120.0)),
                Space::new().width(8),
                pick_list(
                    self.template_manager.get_templates(),
                    self.selected_template.clone(),
                    Message::TemplateSelected,
                )
                .placeholder("Template...")
                .text_size(tiny)
                .width(Length::Fixed(150.0)),
                button(text("Exec").size(tiny))
                    .on_press(Message::ExecuteTemplate)
                    .padding([4, 8])
                    .style(crate::styles::nav_button),
            ]
            .spacing(3)
            .align_y(iced::Alignment::Center),
        )
        .padding([5, 8])
        .width(Length::Fill);

        let copy_progress_bar: Element<'_, Message> = {
            let progress_data = self.copy_progress.lock().ok().and_then(|g| g.clone());
            match progress_data {
                Some(p) if !p.done => {
                    let pct = if p.bytes_total > 0 {
                        (p.bytes_transferred as f32 / p.bytes_total as f32).min(1.0)
                    } else if p.total > 0 {
                        p.current as f32 / p.total as f32
                    } else {
                        0.0
                    };
                    // Build label with byte info if available
                    let label = if p.bytes_total > 0 {
                        format!(
                            "{} {}/{} ({})",
                            p.operation,
                            p.current,
                            p.total,
                            crate::file_types::format_file_size(p.bytes_transferred),
                        )
                    } else {
                        format!("{} {}/{}", p.operation, p.current, p.total)
                    };

                    // Calculate ETA based on bytes if available, else items
                    let elapsed = p.started_at.elapsed();
                    let eta_text = if p.bytes_transferred > 0 && p.bytes_total > 0 {
                        let bytes_per_sec = p.bytes_transferred as f64 / elapsed.as_secs_f64();
                        let remaining_bytes =
                            p.bytes_total.saturating_sub(p.bytes_transferred) as f64;
                        let remaining_secs = remaining_bytes / bytes_per_sec;
                        if remaining_secs < 60.0 {
                            format!(
                                "{}/s ~{}s",
                                crate::file_types::format_file_size(bytes_per_sec as u64),
                                remaining_secs as u64
                            )
                        } else {
                            format!(
                                "{}/s ~{}m{}s",
                                crate::file_types::format_file_size(bytes_per_sec as u64),
                                remaining_secs as u64 / 60,
                                remaining_secs as u64 % 60
                            )
                        }
                    } else if p.current > 0 {
                        let secs_per_item = elapsed.as_secs_f64() / p.current as f64;
                        let remaining = p.total.saturating_sub(p.current) as f64 * secs_per_item;
                        if remaining < 60.0 {
                            format!("~{}s left", remaining as u64)
                        } else {
                            format!(
                                "~{}m {}s left",
                                remaining as u64 / 60,
                                remaining as u64 % 60
                            )
                        }
                    } else {
                        "estimating...".to_string()
                    };

                    let file_display = if p.current_file.len() > 25 {
                        format!(
                            "...{}",
                            &p.current_file[p.current_file.len().saturating_sub(22)..]
                        )
                    } else {
                        p.current_file.clone()
                    };

                    container(
                        row![
                            text(label)
                                .size(tiny)
                                .color(iced::Color::from_rgb(0.4, 0.8, 0.4)),
                            text(file_display)
                                .size(tiny)
                                .width(Length::Fixed(150.0))
                                .color(iced::Color::from_rgb(0.6, 0.6, 0.65)),
                            progress_bar(0.0..=1.0, pct).girth(6.0).length(Length::Fill),
                            text(eta_text)
                                .size(tiny)
                                .color(iced::Color::from_rgb(0.6, 0.6, 0.65)),
                            button(text("Cancel").size(tiny))
                                .on_press(Message::CopyCancel)
                                .padding([2, 8])
                                .style(crate::styles::nav_button),
                        ]
                        .spacing(8)
                        .align_y(iced::Alignment::Center),
                    )
                    .padding([3, 10])
                    .into()
                }
                _ => Space::new().height(0).into(),
            }
        };

        column![
            row![left_pane, rule::vertical(1), right_pane].height(Length::Fill),
            rule::horizontal(1),
            copy_progress_bar,
            function_bar,
        ]
        .into()
    }

    fn view_music_player(&self) -> Element<'_, Message> {
        self.music_player
            .view(self.settings.preferences.font_size)
            .map(Message::MusicPlayer)
    }

    fn view_settings(&self) -> Element<'_, Message> {
        let fs = crate::styles::FontSizes::from_base(self.settings.preferences.font_size);
        let dim = iced::Color::from_rgb(0.55, 0.55, 0.6);
        let header_color = iced::Color::from_rgb(0.7, 0.72, 0.8);

        macro_rules! section {
            ($title:expr, $content:expr) => {
                container(
                    column![
                        text($title).size(fs.large).color(header_color),
                        rule::horizontal(1),
                        Space::new().height(8),
                        $content,
                    ]
                    .spacing(4),
                )
                .padding(15)
                .width(Length::Fill)
                .style(crate::styles::section_style)
            };
        }

        // ── Profiles ─────────────────────────────────────────────────────
        let profile_names = self.profile_manager.profile_names();
        let profile_section = section!(
            "Profiles",
            column![
                row![
                    text("Active:").size(fs.normal).color(dim),
                    pick_list(
                        profile_names,
                        Some(self.profile_manager.active_profile.clone()),
                        Message::ProfileSelected,
                    )
                    .text_size(fs.small as f32)
                    .width(Length::Fixed(180.0)),
                    button(text("Save").size(fs.small))
                        .on_press(Message::SaveProfile)
                        .padding([4, 10])
                        .style(crate::styles::action_button),
                    button(text("Duplicate").size(fs.small))
                        .on_press(Message::DuplicateProfile)
                        .padding([4, 10])
                        .style(crate::styles::nav_button),
                    button(text("Delete").size(fs.small))
                        .on_press(Message::DeleteProfile)
                        .padding([4, 10])
                        .style(crate::styles::nav_button),
                ]
                .spacing(8)
                .align_y(iced::Alignment::Center),
                row![
                    text("New:").size(fs.normal).color(dim),
                    text_input("Profile name...", &self.new_profile_name)
                        .on_input(Message::NewProfileNameChanged)
                        .on_submit(Message::CreateProfile)
                        .padding(6)
                        .size(fs.small as f32)
                        .width(Length::Fixed(180.0)),
                    button(text("Create").size(fs.small))
                        .on_press(Message::CreateProfile)
                        .padding([4, 10])
                        .style(crate::styles::nav_button),
                ]
                .spacing(8)
                .align_y(iced::Alignment::Center),
            ]
            .spacing(8)
        );

        // ── Connection ───────────────────────────────────────────────────
        let discovery_button: Element<'_, Message> = if self.is_discovering {
            button(text("Scanning...").size(fs.small))
                .padding([4, 10])
                .style(crate::styles::nav_button)
                .into()
        } else {
            button(text("Find Devices").size(fs.small))
                .on_press(Message::StartDiscovery)
                .padding([4, 10])
                .style(crate::styles::nav_button)
                .into()
        };

        let discovered_list: Element<'_, Message> = if self.discovered_devices.is_empty() {
            if self.is_discovering {
                text("Scanning network...").size(fs.small).color(dim).into()
            } else {
                Space::new().height(0).into()
            }
        } else {
            column(
                self.discovered_devices
                    .iter()
                    .map(|d| {
                        let device = d.clone();
                        let label = format!("{} - {} ({})", d.ip, d.product, d.firmware);
                        button(text(label).size(fs.small))
                            .on_press(Message::SelectDiscoveredDevice(device))
                            .padding([4, 8])
                            .width(Length::Fill)
                            .style(crate::styles::nav_button)
                            .into()
                    })
                    .collect::<Vec<_>>(),
            )
            .spacing(2)
            .width(Length::Fixed(400.0))
            .into()
        };

        let status_indicator: Element<'_, Message> = if self.status.connected {
            let info_text = self.status.device_info.as_deref().unwrap_or("");
            row![
                text(format!("Connected to {}", self.settings.connection.host))
                    .size(fs.normal)
                    .color(iced::Color::from_rgb(0.3, 0.8, 0.3)),
                text(info_text).size(fs.small).color(dim),
            ]
            .spacing(10)
            .align_y(iced::Alignment::Center)
            .into()
        } else {
            text("Not connected")
                .size(fs.normal)
                .color(iced::Color::from_rgb(0.7, 0.3, 0.3))
                .into()
        };

        let connection_section = section!(
            "Connection",
            column![
                row![
                    text("IP Address:").size(fs.normal).color(dim),
                    text_input("eg. 192.168.1.64", &self.host_input)
                        .on_input(Message::HostInputChanged)
                        .padding(6)
                        .size(fs.small as f32)
                        .width(Length::Fixed(200.0)),
                    discovery_button,
                ]
                .spacing(8)
                .align_y(iced::Alignment::Center),
                discovered_list,
                row![
                    text("Password:").size(fs.normal).color(dim),
                    text_input("optional", &self.password_input)
                        .on_input(Message::PasswordInputChanged)
                        .padding(6)
                        .size(fs.small as f32)
                        .width(Length::Fixed(200.0)),
                ]
                .spacing(8)
                .align_y(iced::Alignment::Center),
                row![
                    text("Stream Control:").size(fs.normal).color(dim),
                    pick_list(
                        &StreamControlMethod::ALL[..],
                        Some(self.settings.connection.stream_control_method),
                        Message::StreamControlMethodChanged,
                    )
                    .text_size(fs.small as f32)
                    .width(Length::Fixed(220.0)),
                ]
                .spacing(8)
                .align_y(iced::Alignment::Center),
                Space::new().height(4),
                row![
                    button(text("Connect").size(fs.small))
                        .on_press(Message::ConnectPressed)
                        .padding([6, 16])
                        .style(crate::styles::action_button),
                    button(text("Disconnect").size(fs.small))
                        .on_press(Message::DisconnectPressed)
                        .padding([6, 16])
                        .style(crate::styles::nav_button),
                    button(text("Test").size(fs.small))
                        .on_press(Message::RefreshStatus)
                        .padding([6, 16])
                        .style(crate::styles::nav_button),
                    Space::new().width(20),
                    status_indicator,
                ]
                .spacing(8)
                .align_y(iced::Alignment::Center),
            ]
            .spacing(8)
        );

        // ── Starting Directories ─────────────────────────────────────────
        let fb_dir = self
            .settings
            .default_paths
            .file_browser_start_dir
            .as_ref()
            .map(|p| p.to_string_lossy().to_string())
            .unwrap_or_else(|| "(home directory)".to_string());
        let mp_dir = self
            .settings
            .default_paths
            .music_player_start_dir
            .as_ref()
            .map(|p| p.to_string_lossy().to_string())
            .unwrap_or_else(|| "(home directory)".to_string());

        let dirs_section = section!(
            "Starting Directories",
            column![
                row![
                    text("File Browser:")
                        .size(fs.normal)
                        .color(dim)
                        .width(Length::Fixed(120.0)),
                    text(fb_dir.clone()).size(fs.small).width(Length::Fill),
                    button(text("Browse").size(fs.small))
                        .on_press(Message::BrowseFileBrowserStartDir)
                        .padding([3, 8])
                        .style(crate::styles::nav_button),
                    button(text("Clear").size(fs.small))
                        .on_press(Message::ClearFileBrowserStartDir)
                        .padding([3, 8])
                        .style(crate::styles::nav_button),
                ]
                .spacing(8)
                .align_y(iced::Alignment::Center),
                row![
                    text("Music Player:")
                        .size(fs.normal)
                        .color(dim)
                        .width(Length::Fixed(120.0)),
                    text(mp_dir.clone()).size(fs.small).width(Length::Fill),
                    button(text("Browse").size(fs.small))
                        .on_press(Message::BrowseMusicPlayerStartDir)
                        .padding([3, 8])
                        .style(crate::styles::nav_button),
                    button(text("Clear").size(fs.small))
                        .on_press(Message::ClearMusicPlayerStartDir)
                        .padding([3, 8])
                        .style(crate::styles::nav_button),
                ]
                .spacing(8)
                .align_y(iced::Alignment::Center),
                text("Changes take effect on next restart")
                    .size(fs.tiny)
                    .color(dim),
            ]
            .spacing(6)
        );

        // ── Preferences ──────────────────────────────────────────────────
        let prefs_section = section!(
            "Preferences",
            column![
                row![
                    text("Default song duration:").size(fs.normal).color(dim),
                    text_input(
                        "180",
                        &self.settings.preferences.default_song_duration.to_string()
                    )
                    .on_input(Message::DefaultSongDurationChanged)
                    .padding(6)
                    .size(fs.small as f32)
                    .width(Length::Fixed(60.0)),
                    text("seconds").size(fs.small).color(dim),
                ]
                .spacing(8)
                .align_y(iced::Alignment::Center),
                row![
                    text("Font size:").size(fs.normal).color(dim),
                    text_input("12", &self.font_size_input)
                        .on_input(Message::FontSizeChanged)
                        .padding(6)
                        .size(fs.small as f32)
                        .width(Length::Fixed(50.0)),
                    text("(8\u{2013}24)").size(fs.small).color(dim),
                ]
                .spacing(8)
                .align_y(iced::Alignment::Center),
            ]
            .spacing(8)
        );

        // ── Debug ────────────────────────────────────────────────────────
        let debug_section = section!(
            "Debug",
            column![text(format!(
                "Platform: {} | Config: {} | Profile: {} ({} total)",
                std::env::consts::OS,
                dirs::config_dir()
                    .map(|p| p.to_string_lossy().to_string())
                    .unwrap_or_default(),
                self.profile_manager.active_profile,
                self.profile_manager.profiles.len(),
            ))
            .size(fs.small)
            .color(dim),]
            .spacing(4)
        );

        scrollable(
            column![
                profile_section,
                connection_section,
                dirs_section,
                prefs_section,
                debug_section,
            ]
            .spacing(10)
            .padding(15)
            .width(Length::Fill),
        )
        .height(Length::Fill)
        .into()
    }

    fn view_status_bar(&self) -> Element<'_, Message> {
        let fs = crate::styles::FontSizes::from_base(self.settings.preferences.font_size);
        let video_status = if self.video_streaming.is_streaming {
            "STREAMING"
        } else {
            "IDLE"
        };

        // Show user message if present, otherwise show video status
        let status_text: Element<'_, Message> = if let Some(msg) = &self.user_message {
            let (prefix, message, is_error) = match msg {
                UserMessage::Error(e) => ("ERROR: ", e.as_str(), true),
                UserMessage::Info(i) => ("", i.as_str(), false),
            };
            let color = if is_error {
                iced::Color::from_rgb(0.8, 0.0, 0.0)
            } else {
                iced::Color::from_rgb(0.0, 0.5, 0.0)
            };

            // Check if this is a screenshot message - make path clickable
            if message.starts_with("Screenshot saved: ") {
                let path = message
                    .strip_prefix("Screenshot saved: ")
                    .unwrap_or(message);
                row![
                    text("Screenshot saved: ").size(fs.normal).color(color),
                    button(
                        text(path)
                            .size(fs.normal)
                            .color(iced::Color::from_rgb(0.3, 0.6, 1.0))
                    )
                    .style(button::text)
                    .on_press(Message::Streaming(StreamingMessage::OpenScreenshot(
                        path.to_string()
                    )))
                    .padding(0),
                    tooltip(
                        button(text("X").size(fs.tiny))
                            .on_press(Message::DismissMessage)
                            .padding([2, 6]),
                        "Dismiss message",
                        tooltip::Position::Top,
                    )
                    .style(container::bordered_box),
                ]
                .spacing(10)
                .align_y(iced::Alignment::Center)
                .into()
            } else {
                let mut row_items: Vec<Element<'_, Message>> =
                    vec![text(format!("{}{}", prefix, message))
                        .size(fs.normal)
                        .color(color)
                        .into()];
                // While a drag-and-drop upload is in flight, expose a
                // Cancel button right next to the status text so the user
                // doesn't have to wait for the timeout if the device is
                // silent. Cancels via `Task::abort()` on the stashed handle.
                if self.drop_in_flight {
                    row_items.push(
                        tooltip(
                            button(text("✕ Cancel").size(fs.tiny))
                                .on_press(Message::DropAbort)
                                .padding([2, 8]),
                            "Cancel the in-flight drop action",
                            tooltip::Position::Top,
                        )
                        .style(container::bordered_box)
                        .into(),
                    );
                }
                row_items.push(
                    tooltip(
                        button(text("X").size(fs.tiny))
                            .on_press(Message::DismissMessage)
                            .padding([2, 6]),
                        "Dismiss message",
                        tooltip::Position::Top,
                    )
                    .style(container::bordered_box)
                    .into(),
                );
                iced::widget::Row::with_children(row_items)
                    .spacing(10)
                    .align_y(iced::Alignment::Center)
                    .into()
            }
        } else {
            text(video_status).size(fs.normal).into()
        };

        let connected = self.status.connected;

        container(
            row![
                status_text,
                Space::new().width(Length::Fill),
                tooltip(
                    button(text("MENU").size(fs.small))
                        .on_press_maybe(connected.then_some(Message::MenuButton))
                        .padding([4, 8]),
                    "Press Ultimate64 menu button",
                    tooltip::Position::Top,
                )
                .style(container::bordered_box),
                text("|").size(fs.normal),
                tooltip(
                    button(text("PAUSE").size(fs.small))
                        .on_press_maybe(connected.then_some(Message::PauseMachine))
                        .padding([4, 8]),
                    "Pause the C64 CPU",
                    tooltip::Position::Top,
                )
                .style(container::bordered_box),
                tooltip(
                    button(text("RESUME").size(fs.small))
                        .on_press_maybe(connected.then_some(Message::ResumeMachine))
                        .padding([4, 8]),
                    "Resume the C64 CPU",
                    tooltip::Position::Top,
                )
                .style(container::bordered_box),
                text("|").size(fs.normal),
                tooltip(
                    button(text("RESET").size(fs.small))
                        .on_press_maybe(connected.then_some(Message::ResetMachine))
                        .padding([4, 8]),
                    "Reset the C64 (soft reset)",
                    tooltip::Position::Top,
                )
                .style(container::bordered_box),
                tooltip(
                    button(text("REBOOT").size(fs.small))
                        .on_press_maybe(connected.then_some(Message::RebootMachine))
                        .padding([4, 8]),
                    "Reboot the Ultimate64 device",
                    tooltip::Position::Top,
                )
                .style(container::bordered_box),
                tooltip(
                    button(text("POWER OFF").size(fs.small))
                        .on_press_maybe(connected.then_some(Message::PoweroffMachine))
                        .padding([4, 8]),
                    "Power off the Ultimate64",
                    tooltip::Position::Top,
                )
                .style(container::bordered_box),
            ]
            .spacing(6)
            .align_y(iced::Alignment::Center),
        )
        .padding([8, 15])
        .into()
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
                self.connection = Some(Arc::new(TokioMutex::new(rest)));
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
    connection: Arc<TokioMutex<Rest>>,
    commands: Vec<String>,
) -> Result<(), String> {
    for command in commands {
        log::info!("Executing: {}", command);
        let conn = connection.clone();

        if command.starts_with("RESET") {
            let result = tokio::time::timeout(
                tokio::time::Duration::from_secs(REST_TIMEOUT_SECS),
                tokio::task::spawn_blocking(move || {
                    let conn = conn.blocking_lock();
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
                    let conn = conn.blocking_lock();
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
                    let conn = conn.blocking_lock();
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
                    let conn = conn.blocking_lock();
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

async fn fetch_status(connection: Arc<TokioMutex<Rest>>) -> Result<StatusInfo, String> {
    // Use spawn_blocking to avoid runtime conflicts with ultimate64 crate
    // Wrap in timeout to prevent hangs when device is offline
    let result = tokio::time::timeout(
        tokio::time::Duration::from_secs(REST_TIMEOUT_SECS),
        tokio::task::spawn_blocking(move || {
            let conn = connection.blocking_lock();

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
