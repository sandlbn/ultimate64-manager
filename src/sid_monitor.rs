//! Live hardware register monitor for the Commodore 64.
//!
//! Polls C64 memory via the Ultimate64 REST API and decodes the raw register
//! values into human-readable form for three hardware chips:
//!
//! - **SID** ($D400 / $D500 / $D440) — 3 voices, filter, envelope, waveforms
//! - **VIC-II** ($D000) — video, sprites, raster, colour registers
//! - **CIA #1 / #2** ($DC00 / $DD00) — timers, TOD clock, port states
//!

use iced::{
    widget::{button, column, container, pick_list, row, rule, scrollable, text, Column, Space},
    Element, Length, Subscription, Task,
};
use std::sync::Arc;
use tokio::sync::Mutex as TokioMutex;
use ultimate64::Rest;

// ─────────────────────────────────────────────────────────────────
//  Hardware constants
// ─────────────────────────────────────────────────────────────────

/// Default poll interval in milliseconds
const POLL_MS: u64 = 250;

/// REST read timeout
const TIMEOUT_SECS: u64 = 3;

/// PAL clock frequency used for SID frequency → Hz conversion
const PAL_CLOCK: f64 = 985_248.4;
const NTSC_CLOCK: f64 = 1_022_727.1;

/// SID register block size (29 registers, padded to 32)
const SID_REG_LEN: u16 = 0x1C;

/// VIC-II base address and register count
const VIC_BASE: u16 = 0xD000;
const VIC_REG_LEN: u16 = 0x40;

/// CIA register block size
const CIA_REG_LEN: u16 = 0x10;
const CIA1_BASE: u16 = 0xDC00;
const CIA2_BASE: u16 = 0xDD00;

/// C64 colour palette (Colodore, 16 entries)
const PALETTE: [(u8, u8, u8, &str); 16] = [
    (0x00, 0x00, 0x00, "Black"),
    (0xEF, 0xEF, 0xEF, "White"),
    (0x8D, 0x2F, 0x34, "Red"),
    (0x6A, 0xD4, 0xCD, "Cyan"),
    (0x98, 0x35, 0xA4, "Purple"),
    (0x4C, 0xB4, 0x42, "Green"),
    (0x2C, 0x29, 0xB1, "Blue"),
    (0xEF, 0xEF, 0x5D, "Yellow"),
    (0x98, 0x4E, 0x20, "Orange"),
    (0x5B, 0x38, 0x00, "Brown"),
    (0xD1, 0x67, 0x6D, "Light Red"),
    (0x4A, 0x4A, 0x4A, "Dark Grey"),
    (0x7B, 0x7B, 0x7B, "Mid Grey"),
    (0x9F, 0xEF, 0x93, "Light Green"),
    (0x6D, 0x6A, 0xEF, "Light Blue"),
    (0xB2, 0xB2, 0xB2, "Light Grey"),
];

/// ADSR timing table (index = nibble value, value = human label)
const ADSR_TIME: [&str; 16] = [
    "2ms", "8ms", "16ms", "24ms", "38ms", "56ms", "68ms", "80ms", "100ms", "250ms", "500ms",
    "800ms", "1s", "3s", "5s", "8s",
];

// ─────────────────────────────────────────────────────────────────
//  Types
// ─────────────────────────────────────────────────────────────────

/// Which panel is active
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum MonitorPanel {
    #[default]
    Sid,
    Vic,
    Cia,
}

impl std::fmt::Display for MonitorPanel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            MonitorPanel::Sid => write!(f, "SID"),
            MonitorPanel::Vic => write!(f, "VIC-II"),
            MonitorPanel::Cia => write!(f, "CIA"),
        }
    }
}

/// Which SID chip to read (address)
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SidAddress {
    #[default]
    D400, // SID #1 (always present)
    D500, // SID #2 — most common stereo address (Prophet64, HardSID)
    D420, // SID #2 — alternative (SidCard compatible)
    DE00, // SID #2 — I/O expansion area
    DF00, // SID #2 — I/O expansion area alt
    D440, // SID #3
}

impl SidAddress {
    pub fn base(self) -> u16 {
        match self {
            SidAddress::D400 => 0xD400,
            SidAddress::D500 => 0xD500,
            SidAddress::D420 => 0xD420,
            SidAddress::DE00 => 0xDE00,
            SidAddress::DF00 => 0xDF00,
            SidAddress::D440 => 0xD440,
        }
    }
}

impl std::fmt::Display for SidAddress {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "${:04X}", self.base())
    }
}

/// TV standard — affects SID frequency → Hz conversion
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum TvStandard {
    #[default]
    Pal,
    Ntsc,
}

impl std::fmt::Display for TvStandard {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            TvStandard::Pal => write!(f, "PAL"),
            TvStandard::Ntsc => write!(f, "NTSC"),
        }
    }
}

// ─────────────────────────────────────────────────────────────────
//  Decoded SID state (one chip)
// ─────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Default)]
struct SidVoice {
    freq_reg: u16,
    pw_reg: u16,
    control: u8,
    attack: u8,
    decay: u8,
    sustain: u8,
    release: u8,
}

impl SidVoice {
    fn from_regs(r: &[u8], base: usize) -> Self {
        Self {
            freq_reg: (r[base] as u16) | ((r[base + 1] as u16) << 8),
            pw_reg: (r[base + 2] as u16) | ((r[base + 3] as u16 & 0x0F) << 8),
            control: r[base + 4],
            attack: r[base + 5] >> 4,
            decay: r[base + 5] & 0x0F,
            sustain: r[base + 6] >> 4,
            release: r[base + 6] & 0x0F,
        }
    }

    fn freq_hz(&self, clock: f64) -> f64 {
        self.freq_reg as f64 * clock / 16_777_216.0
    }

    fn note_name(&self, clock: f64) -> String {
        let hz = self.freq_hz(clock);
        if hz < 16.0 {
            return "---".into();
        }
        let midi = (69.0 + 12.0 * (hz / 440.0).log2()).round() as i32;
        let names = [
            "C", "C#", "D", "D#", "E", "F", "F#", "G", "G#", "A", "A#", "B",
        ];
        let note = ((midi % 12) + 12) as usize % 12;
        let oct = (midi / 12) - 1;
        format!("{}{}", names[note], oct)
    }

    fn waveform(&self) -> String {
        let c = self.control;
        let mut w = Vec::new();
        if c & 0x80 != 0 {
            w.push("NOISE");
        }
        if c & 0x40 != 0 {
            w.push("PULSE");
        }
        if c & 0x20 != 0 {
            w.push("SAW");
        }
        if c & 0x10 != 0 {
            w.push("TRI");
        }
        if w.is_empty() {
            "---".into()
        } else {
            w.join("+")
        }
    }

    fn gate(&self) -> bool {
        self.control & 0x01 != 0
    }
    fn sync(&self) -> bool {
        self.control & 0x02 != 0
    }
    fn ring(&self) -> bool {
        self.control & 0x04 != 0
    }
    fn test(&self) -> bool {
        self.control & 0x08 != 0
    }
}

#[derive(Debug, Clone, Default)]
struct SidState {
    voices: [SidVoice; 3],
    fc_lo: u8,    // filter cutoff low 3 bits
    fc_hi: u8,    // filter cutoff high 8 bits
    res_filt: u8, // resonance + filter routing
    mode_vol: u8, // mode flags + volume
    osc3: u8,     // voice 3 oscillator output (read-only)
    env3: u8,     // voice 3 envelope output (read-only)
}

impl SidState {
    fn from_bytes(r: &[u8]) -> Self {
        if r.len() < 0x1C {
            return Self::default();
        }
        Self {
            voices: [
                SidVoice::from_regs(r, 0x00),
                SidVoice::from_regs(r, 0x07),
                SidVoice::from_regs(r, 0x0E),
            ],
            fc_lo: r[0x15],
            fc_hi: r[0x16],
            res_filt: r[0x17],
            mode_vol: r[0x18],
            osc3: if r.len() > 0x1B { r[0x1B] } else { 0 },
            env3: if r.len() > 0x1C { r[0x1C] } else { 0 },
        }
    }

    fn filter_cutoff(&self) -> u16 {
        (self.fc_lo as u16 & 0x07) | ((self.fc_hi as u16) << 3)
    }

    fn filter_cutoff_hz(&self, clock: f64) -> f64 {
        // Approximation: fc_hz ≈ (cutoff / 2047) * (clock / 20)
        (self.filter_cutoff() as f64 / 2047.0) * (clock / 20.0)
    }

    fn resonance(&self) -> u8 {
        self.res_filt >> 4
    }
    fn filt_ex(&self) -> bool {
        self.res_filt & 0x01 != 0
    }
    fn filt_1(&self) -> bool {
        self.res_filt & 0x02 != 0
    }
    fn filt_2(&self) -> bool {
        self.res_filt & 0x04 != 0
    }
    fn filt_3(&self) -> bool {
        self.res_filt & 0x08 != 0
    }
    fn volume(&self) -> u8 {
        self.mode_vol & 0x0F
    }
    fn lp(&self) -> bool {
        self.mode_vol & 0x10 != 0
    }
    fn bp(&self) -> bool {
        self.mode_vol & 0x20 != 0
    }
    fn hp(&self) -> bool {
        self.mode_vol & 0x40 != 0
    }
    fn mute_v3(&self) -> bool {
        self.mode_vol & 0x80 != 0
    }
}

// ─────────────────────────────────────────────────────────────────
//  Decoded VIC-II state
// ─────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Default)]
struct VicState {
    raw: Vec<u8>,
}

impl VicState {
    fn from_bytes(r: Vec<u8>) -> Self {
        Self { raw: r }
    }

    fn byte(&self, offset: usize) -> u8 {
        self.raw.get(offset).copied().unwrap_or(0)
    }

    fn raster(&self) -> u16 {
        let hi = if self.byte(0x11) & 0x80 != 0 {
            0x100
        } else {
            0
        };
        hi | self.byte(0x12) as u16
    }

    fn border_colour(&self) -> u8 {
        self.byte(0x20) & 0x0F
    }
    fn bg_colour(&self) -> u8 {
        self.byte(0x21) & 0x0F
    }
    fn screen_enabled(&self) -> bool {
        self.byte(0x11) & 0x10 != 0
    }
    fn bitmap_mode(&self) -> bool {
        self.byte(0x11) & 0x20 != 0
    }
    fn ecm_mode(&self) -> bool {
        self.byte(0x11) & 0x40 != 0
    }
    fn mcm_mode(&self) -> bool {
        self.byte(0x16) & 0x10 != 0
    }
    fn sprites_enabled(&self) -> u8 {
        self.byte(0x15)
    }

    fn video_bank(&self) -> u8 {
        3 - (self.byte(0x18) >> 4)
    } // CIA2 port A inverted
    fn char_base(&self) -> u16 {
        let ptr = (self.byte(0x18) >> 1) & 0x07;
        (self.video_bank() as u16 * 0x4000) + (ptr as u16 * 0x0800)
    }
    fn screen_base(&self) -> u16 {
        let ptr = self.byte(0x18) >> 4;
        (self.video_bank() as u16 * 0x4000) + (ptr as u16 * 0x0400)
    }

    fn colour_name(index: u8) -> &'static str {
        PALETTE
            .get(index as usize & 0x0F)
            .map(|p| p.3)
            .unwrap_or("?")
    }

    fn colour_rgb(index: u8) -> iced::Color {
        let (r, g, b, _) = PALETTE[index as usize & 0x0F];
        iced::Color::from_rgb(r as f32 / 255.0, g as f32 / 255.0, b as f32 / 255.0)
    }
}

// ─────────────────────────────────────────────────────────────────
//  Decoded CIA state
// ─────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Default)]
struct CiaState {
    raw: Vec<u8>,
    base_addr: u16,
}

impl CiaState {
    fn from_bytes(raw: Vec<u8>, base: u16) -> Self {
        Self {
            raw,
            base_addr: base,
        }
    }

    fn byte(&self, offset: usize) -> u8 {
        self.raw.get(offset).copied().unwrap_or(0)
    }

    fn timer_a(&self) -> u16 {
        (self.byte(0x04) as u16) | ((self.byte(0x05) as u16) << 8)
    }
    fn timer_b(&self) -> u16 {
        (self.byte(0x06) as u16) | ((self.byte(0x07) as u16) << 8)
    }

    fn tod_hours(&self) -> u8 {
        self.byte(0x0B) & 0x1F
    }
    fn tod_minutes(&self) -> u8 {
        self.byte(0x0A)
    }
    fn tod_seconds(&self) -> u8 {
        self.byte(0x09)
    }
    fn tod_tenths(&self) -> u8 {
        self.byte(0x08)
    }
    fn tod_pm(&self) -> bool {
        self.byte(0x0B) & 0x80 != 0
    }

    fn tod_string(&self) -> String {
        // BCD decoding
        let bcd = |v: u8| ((v >> 4) * 10 + (v & 0x0F)) as u32;
        format!(
            "{:02}:{:02}:{:02}.{} {}",
            bcd(self.tod_hours()),
            bcd(self.tod_minutes()),
            bcd(self.tod_seconds()),
            bcd(self.tod_tenths()),
            if self.tod_pm() { "PM" } else { "AM" }
        )
    }

    fn icr(&self) -> u8 {
        self.byte(0x0D)
    }
    fn cra(&self) -> u8 {
        self.byte(0x0E)
    }
    fn crb(&self) -> u8 {
        self.byte(0x0F)
    }

    fn timer_a_running(&self) -> bool {
        self.cra() & 0x01 != 0
    }
    fn timer_b_running(&self) -> bool {
        self.crb() & 0x01 != 0
    }

    fn irq_set(&self) -> bool {
        self.icr() & 0x80 != 0
    }
    fn irq_ta(&self) -> bool {
        self.icr() & 0x01 != 0
    }
    fn irq_tb(&self) -> bool {
        self.icr() & 0x02 != 0
    }
    fn irq_tod(&self) -> bool {
        self.icr() & 0x04 != 0
    }
    fn irq_sp(&self) -> bool {
        self.icr() & 0x08 != 0
    }
    fn irq_flag(&self) -> bool {
        self.icr() & 0x10 != 0
    }
}

// ─────────────────────────────────────────────────────────────────
//  Messages
// ─────────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub enum SidMonitorMessage {
    // Panel navigation
    PanelChanged(MonitorPanel),

    // SID settings
    SidAddressChanged(SidAddress),
    TvStandardChanged(TvStandard),

    // Watch control
    ToggleWatch,
    Tick,

    // Data received
    SidDataReceived(Result<Vec<u8>, String>),
    VicDataReceived(Result<Vec<u8>, String>),
    CiaDataReceived(Result<(Vec<u8>, Vec<u8>), String>),
}

// ─────────────────────────────────────────────────────────────────
//  State
// ─────────────────────────────────────────────────────────────────

pub struct SidMonitor {
    // Settings
    active_panel: MonitorPanel,
    sid_address: SidAddress,
    tv_standard: TvStandard,

    // Live data
    sid: Option<SidState>,
    vic: Option<VicState>,
    cia1: Option<CiaState>,
    cia2: Option<CiaState>,

    // Watch
    watch_active: bool,
    is_loading: bool,
    poll_count: u64, // how many polls have completed
    status: String,
}

impl SidMonitor {
    pub fn new() -> Self {
        Self {
            active_panel: MonitorPanel::Sid,
            sid_address: SidAddress::D400,
            tv_standard: TvStandard::Pal,
            sid: None,
            vic: None,
            cia1: None,
            cia2: None,
            watch_active: false,
            is_loading: false,
            poll_count: 0,
            status: String::new(),
        }
    }

    pub fn subscription(&self) -> Subscription<SidMonitorMessage> {
        if self.watch_active {
            iced::time::every(std::time::Duration::from_millis(POLL_MS))
                .map(|_| SidMonitorMessage::Tick)
        } else {
            Subscription::none()
        }
    }

    pub fn update(
        &mut self,
        message: SidMonitorMessage,
        connection: Option<Arc<TokioMutex<Rest>>>,
    ) -> Task<SidMonitorMessage> {
        match message {
            SidMonitorMessage::PanelChanged(p) => {
                self.active_panel = p;
                Task::none()
            }

            SidMonitorMessage::SidAddressChanged(addr) => {
                self.sid_address = addr;
                self.sid = None;
                Task::none()
            }

            SidMonitorMessage::TvStandardChanged(tv) => {
                self.tv_standard = tv;
                Task::none()
            }

            SidMonitorMessage::ToggleWatch => {
                self.watch_active = !self.watch_active;
                self.status = if self.watch_active {
                    format!("Live — polling every {} ms", POLL_MS)
                } else {
                    "Stopped".into()
                };
                // Trigger an immediate read when starting
                if self.watch_active {
                    return self.update(SidMonitorMessage::Tick, connection);
                }
                Task::none()
            }

            SidMonitorMessage::Tick => {
                if self.is_loading {
                    return Task::none();
                }
                let Some(conn) = connection else {
                    return Task::none();
                };
                self.is_loading = true;

                match self.active_panel {
                    MonitorPanel::Sid => {
                        let addr = self.sid_address.base();
                        Task::perform(
                            async move { read_bytes(conn, addr, SID_REG_LEN).await },
                            SidMonitorMessage::SidDataReceived,
                        )
                    }
                    MonitorPanel::Vic => Task::perform(
                        async move { read_bytes(conn, VIC_BASE, VIC_REG_LEN).await },
                        SidMonitorMessage::VicDataReceived,
                    ),
                    MonitorPanel::Cia => Task::perform(
                        async move {
                            let r1 = read_bytes(conn.clone(), CIA1_BASE, CIA_REG_LEN).await;
                            let r2 = read_bytes(conn, CIA2_BASE, CIA_REG_LEN).await;
                            match (r1, r2) {
                                (Ok(a), Ok(b)) => Ok((a, b)),
                                (Err(e), _) | (_, Err(e)) => Err(e),
                            }
                        },
                        SidMonitorMessage::CiaDataReceived,
                    ),
                }
            }

            SidMonitorMessage::SidDataReceived(result) => {
                self.is_loading = false;
                match result {
                    Ok(data) => {
                        self.poll_count += 1;
                        self.sid = Some(SidState::from_bytes(&data));
                    }
                    Err(e) => {
                        self.status = format!("Read error: {}", e);
                        self.watch_active = false;
                    }
                }
                Task::none()
            }

            SidMonitorMessage::VicDataReceived(result) => {
                self.is_loading = false;
                match result {
                    Ok(data) => {
                        self.poll_count += 1;
                        self.vic = Some(VicState::from_bytes(data));
                    }
                    Err(e) => {
                        self.status = format!("Read error: {}", e);
                        self.watch_active = false;
                    }
                }
                Task::none()
            }

            SidMonitorMessage::CiaDataReceived(result) => {
                self.is_loading = false;
                match result {
                    Ok((d1, d2)) => {
                        self.poll_count += 1;
                        self.cia1 = Some(CiaState::from_bytes(d1, CIA1_BASE));
                        self.cia2 = Some(CiaState::from_bytes(d2, CIA2_BASE));
                    }
                    Err(e) => {
                        self.status = format!("Read error: {}", e);
                        self.watch_active = false;
                    }
                }
                Task::none()
            }
        }
    }

    // ─────────────────────────────────────────────────────────────
    //  View
    // ─────────────────────────────────────────────────────────────

    pub fn view(&self, is_connected: bool, font_size: u32) -> Element<'_, SidMonitorMessage> {
        let sf = font_size.saturating_sub(2);

        if !is_connected {
            return container(
                column![
                    Space::new().height(Length::Fill),
                    text("Please connect to your Ultimate64 device first.").size(sf),
                    Space::new().height(Length::Fill),
                ]
                .align_x(iced::Alignment::Center)
                .width(Length::Fill),
            )
            .width(Length::Fill)
            .height(Length::Fill)
            .padding(10)
            .into();
        }

        let toolbar = self.view_toolbar(sf);
        let panel: Element<'_, SidMonitorMessage> = match self.active_panel {
            MonitorPanel::Sid => self.view_sid(sf),
            MonitorPanel::Vic => self.view_vic(sf),
            MonitorPanel::Cia => self.view_cia(sf),
        };

        container(
            column![toolbar, rule::horizontal(1), panel]
                .spacing(8)
                .width(Length::Fill)
                .height(Length::Fill),
        )
        .width(Length::Fill)
        .height(Length::Fill)
        .padding(10)
        .into()
    }

    // ── Toolbar ──────────────────────────────────────────────────

    fn view_toolbar(&self, sf: u32) -> Element<'_, SidMonitorMessage> {
        let panel_picker = pick_list(
            vec![MonitorPanel::Sid, MonitorPanel::Vic, MonitorPanel::Cia],
            Some(self.active_panel),
            SidMonitorMessage::PanelChanged,
        )
        .text_size(sf)
        .width(Length::Fixed(90.0));

        let watch_btn = button(
            text(if self.watch_active {
                "⏹ Stop"
            } else {
                "▶ Live"
            })
            .size(sf),
        )
        .on_press(SidMonitorMessage::ToggleWatch)
        .style(if self.watch_active {
            button::primary
        } else {
            button::secondary
        })
        .padding([5, 12]);

        let poll_label = if self.poll_count > 0 {
            text(format!("#{}", self.poll_count))
                .size(sf.saturating_sub(1))
                .color(iced::Color::from_rgb(0.5, 0.5, 0.5))
        } else {
            text("").size(sf)
        };

        // SID-specific controls
        let sid_controls: Element<'_, SidMonitorMessage> = if self.active_panel == MonitorPanel::Sid
        {
            row![
                text("SID:").size(sf),
                pick_list(
                    vec![
                        SidAddress::D400,
                        SidAddress::D500,
                        SidAddress::D420,
                        SidAddress::DE00,
                        SidAddress::DF00,
                        SidAddress::D440,
                    ],
                    Some(self.sid_address),
                    SidMonitorMessage::SidAddressChanged,
                )
                .text_size(sf)
                .width(Length::Fixed(80.0)),
                pick_list(
                    vec![TvStandard::Pal, TvStandard::Ntsc],
                    Some(self.tv_standard),
                    SidMonitorMessage::TvStandardChanged,
                )
                .text_size(sf)
                .width(Length::Fixed(65.0)),
            ]
            .spacing(8)
            .align_y(iced::Alignment::Center)
            .into()
        } else {
            Space::new().width(Length::Shrink).into()
        };

        let status_text = if !self.status.is_empty() {
            text(&*self.status)
                .size(sf.saturating_sub(1))
                .color(iced::Color::from_rgb(0.5, 0.5, 0.6))
        } else {
            text("").size(sf)
        };

        row![
            panel_picker,
            Space::new().width(Length::Fixed(12.0)),
            watch_btn,
            poll_label,
            Space::new().width(Length::Fixed(12.0)),
            sid_controls,
            Space::new().width(Length::Fill),
            status_text,
        ]
        .spacing(8)
        .align_y(iced::Alignment::Center)
        .into()
    }

    // ── SID panel ────────────────────────────────────────────────

    fn view_sid(&self, sf: u32) -> Element<'_, SidMonitorMessage> {
        let mf = sf.saturating_sub(1);

        let Some(sid) = &self.sid else {
            return self.view_placeholder("No SID data yet — press ▶ Live", sf);
        };

        let clock = match self.tv_standard {
            TvStandard::Pal => PAL_CLOCK,
            TvStandard::Ntsc => NTSC_CLOCK,
        };

        // ── Filter section ───────────────────────────────────────
        let fc = sid.filter_cutoff();
        let fc_hz = sid.filter_cutoff_hz(clock);
        let mut filter_routing = Vec::new();
        if sid.filt_1() {
            filter_routing.push("V1");
        }
        if sid.filt_2() {
            filter_routing.push("V2");
        }
        if sid.filt_3() {
            filter_routing.push("V3");
        }
        if sid.filt_ex() {
            filter_routing.push("EXT");
        }
        let mut filter_mode = Vec::new();
        if sid.lp() {
            filter_mode.push("LP");
        }
        if sid.bp() {
            filter_mode.push("BP");
        }
        if sid.hp() {
            filter_mode.push("HP");
        }

        let filter_row = container(
            row![
                kv("Cutoff", &format!("${:03X}  ({:.0} Hz)", fc, fc_hz), mf),
                Space::new().width(Length::Fixed(16.0)),
                kv("Res", &format!("{}", sid.resonance()), mf),
                Space::new().width(Length::Fixed(16.0)),
                kv(
                    "Mode",
                    &if filter_mode.is_empty() {
                        "OFF".into()
                    } else {
                        filter_mode.join("+")
                    },
                    mf
                ),
                Space::new().width(Length::Fixed(16.0)),
                kv(
                    "Route",
                    &if filter_routing.is_empty() {
                        "none".into()
                    } else {
                        filter_routing.join(" ")
                    },
                    mf
                ),
                Space::new().width(Length::Fixed(16.0)),
                kv("Vol", &format!("{}", sid.volume()), mf),
                Space::new().width(Length::Fixed(16.0)),
                kv("MuteV3", &yn(sid.mute_v3()), mf),
            ]
            .spacing(4)
            .align_y(iced::Alignment::Center),
        )
        .style(section_style)
        .padding(8)
        .width(Length::Fill);

        // ── Voice table ──────────────────────────────────────────
        let header = voice_header_row(mf);

        let mut voice_rows: Vec<Element<'_, SidMonitorMessage>> = vec![header];
        for (i, voice) in sid.voices.iter().enumerate() {
            let hz = voice.freq_hz(clock);
            let note = voice.note_name(clock);

            let gate_colour = if voice.gate() {
                iced::Color::from_rgb(0.3, 0.9, 0.3)
            } else {
                iced::Color::from_rgb(0.5, 0.5, 0.5)
            };

            let row_el = row![
                // Voice number
                text(format!("V{}", i + 1))
                    .size(mf)
                    .width(Length::Fixed(24.0))
                    .color(iced::Color::from_rgb(0.5, 0.7, 1.0)),
                // Freq reg + Hz + note
                text(format!("${:04X}", voice.freq_reg))
                    .size(mf)
                    .width(Length::Fixed(54.0)),
                text(format!("{:7.1}Hz", hz))
                    .size(mf)
                    .width(Length::Fixed(80.0)),
                text(note)
                    .size(mf)
                    .width(Length::Fixed(36.0))
                    .color(iced::Color::from_rgb(0.9, 0.9, 0.4)),
                // Waveform
                text(voice.waveform()).size(mf).width(Length::Fixed(90.0)),
                // PW (only meaningful for PULSE)
                text(format!("PW ${:03X}", voice.pw_reg))
                    .size(mf)
                    .width(Length::Fixed(72.0))
                    .color(if voice.control & 0x40 != 0 {
                        iced::Color::WHITE
                    } else {
                        iced::Color::from_rgb(0.4, 0.4, 0.4)
                    }),
                // ADSR
                text(format!(
                    "A:{} D:{} S:{} R:{}",
                    ADSR_TIME[voice.attack as usize],
                    ADSR_TIME[voice.decay as usize],
                    voice.sustain,
                    ADSR_TIME[voice.release as usize]
                ))
                .size(mf)
                .width(Length::Fixed(200.0)),
                // Gate
                text(if voice.gate() { "GATE▲" } else { "GATE▼" })
                    .size(mf)
                    .width(Length::Fixed(52.0))
                    .color(gate_colour),
                // Modifiers
                text(format!(
                    "{}{}{}",
                    if voice.sync() { "SYNC " } else { "" },
                    if voice.ring() { "RING " } else { "" },
                    if voice.test() { "TEST" } else { "" }
                ))
                .size(mf),
            ]
            .spacing(6)
            .align_y(iced::Alignment::Center);

            voice_rows.push(row_el.into());
        }

        // V3 read-back row
        voice_rows.push(
            row![
                text("V3 RAW")
                    .size(mf.saturating_sub(1))
                    .color(iced::Color::from_rgb(0.5, 0.5, 0.6))
                    .width(Length::Fixed(24.0)),
                text(format!("Osc=${:02X}", sid.osc3))
                    .size(mf.saturating_sub(1))
                    .color(iced::Color::from_rgb(0.5, 0.5, 0.6)),
                Space::new().width(Length::Fixed(8.0)),
                text(format!("Env=${:02X}", sid.env3))
                    .size(mf.saturating_sub(1))
                    .color(iced::Color::from_rgb(0.5, 0.5, 0.6)),
            ]
            .spacing(6)
            .into(),
        );

        let voices_block = container(Column::with_children(voice_rows).spacing(6))
            .style(section_style)
            .padding(8)
            .width(Length::Fill);

        scrollable(
            column![
                text(format!("SID @ {} — {}", self.sid_address, self.tv_standard)).size(sf),
                rule::horizontal(1),
                voices_block,
                rule::horizontal(1),
                filter_row,
            ]
            .spacing(8)
            .width(Length::Fill),
        )
        .height(Length::Fill)
        .into()
    }

    // ── VIC-II panel ─────────────────────────────────────────────

    fn view_vic(&self, sf: u32) -> Element<'_, SidMonitorMessage> {
        let mf = sf.saturating_sub(1);

        let Some(vic) = &self.vic else {
            return self.view_placeholder("No VIC-II data yet — press ▶ Live", sf);
        };

        // Screen mode string
        let mode = match (vic.ecm_mode(), vic.bitmap_mode(), vic.mcm_mode()) {
            (false, false, false) => "Standard Text",
            (false, false, true) => "Multicolour Text",
            (false, true, false) => "Hires Bitmap",
            (false, true, true) => "Multicolour Bitmap",
            (true, false, false) => "ECM Text",
            _ => "Invalid Mode",
        };

        let screen_row = container(
            row![
                kv("Mode", mode, mf),
                Space::new().width(16),
                kv("Screen", &yn(vic.screen_enabled()), mf),
                Space::new().width(16),
                kv(
                    "Raster",
                    &format!("${:03X} ({})", vic.raster(), vic.raster()),
                    mf
                ),
                Space::new().width(16),
                kv(
                    "VidBank",
                    &format!(
                        "{} (${:04X}–${:04X})",
                        vic.video_bank(),
                        vic.video_bank() as u16 * 0x4000,
                        vic.video_bank() as u16 * 0x4000 + 0x3FFF,
                    ),
                    mf
                ),
            ]
            .spacing(8)
            .align_y(iced::Alignment::Center),
        )
        .style(section_style)
        .padding(8)
        .width(Length::Fill);

        let memory_row = container(
            row![
                kv("Screen RAM", &format!("${:04X}", vic.screen_base()), mf),
                Space::new().width(16),
                kv("Char/Bitmap", &format!("${:04X}", vic.char_base()), mf),
                Space::new().width(16),
                kv("Mem ptr $D018", &format!("${:02X}", vic.byte(0x18)), mf),
            ]
            .spacing(8)
            .align_y(iced::Alignment::Center),
        )
        .style(section_style)
        .padding(8)
        .width(Length::Fill);

        // Colour swatches
        let colour_row = container(
            row![
                text("Border:").size(mf).width(Length::Fixed(50.0)),
                colour_swatch(vic.border_colour(), mf),
                Space::new().width(16),
                text("BG0:").size(mf).width(Length::Fixed(36.0)),
                colour_swatch(vic.bg_colour(), mf),
                Space::new().width(16),
                text("BG1:").size(mf).width(Length::Fixed(36.0)),
                colour_swatch(vic.byte(0x22) & 0x0F, mf),
                Space::new().width(16),
                text("BG2:").size(mf).width(Length::Fixed(36.0)),
                colour_swatch(vic.byte(0x23) & 0x0F, mf),
                Space::new().width(16),
                text("BG3:").size(mf).width(Length::Fixed(36.0)),
                colour_swatch(vic.byte(0x24) & 0x0F, mf),
            ]
            .spacing(4)
            .align_y(iced::Alignment::Center),
        )
        .style(section_style)
        .padding(8)
        .width(Length::Fill);

        // Sprite summary
        let spr_enabled = vic.sprites_enabled();
        let mut spr_rows: Vec<Element<'_, SidMonitorMessage>> = Vec::new();
        for i in 0..8u8 {
            if spr_enabled & (1 << i) != 0 {
                let x_msb = if vic.byte(0x10) & (1 << i) != 0 {
                    0x100u16
                } else {
                    0
                };
                let sx = x_msb | vic.byte((i * 2) as usize) as u16;
                let sy = vic.byte((i * 2 + 1) as usize);
                let colour = vic.byte(0x27 + i as usize) & 0x0F;
                let dblx = vic.byte(0x1D) & (1 << i) != 0;
                let dbly = vic.byte(0x17) & (1 << i) != 0;
                let mc = vic.byte(0x1C) & (1 << i) != 0;
                let bgpri = vic.byte(0x1B) & (1 << i) != 0;
                spr_rows.push(
                    row![
                        text(format!("S{}", i))
                            .size(mf)
                            .color(iced::Color::from_rgb(0.5, 0.7, 1.0))
                            .width(Length::Fixed(20.0)),
                        text(format!("X={:3} Y={:3}", sx, sy))
                            .size(mf)
                            .width(Length::Fixed(100.0)),
                        colour_swatch(colour, mf),
                        Space::new().width(Length::Fixed(4.0)),
                        text(format!(
                            "{}{}{}{}",
                            if dblx { "2X " } else { "" },
                            if dbly { "2Y " } else { "" },
                            if mc { "MC " } else { "" },
                            if bgpri { "BG" } else { "" },
                        ))
                        .size(mf),
                    ]
                    .spacing(6)
                    .align_y(iced::Alignment::Center)
                    .into(),
                );
            }
        }

        let sprites_block = container(
            if spr_rows.is_empty() {
                column![text("No sprites enabled")
                    .size(mf)
                    .color(iced::Color::from_rgb(0.4, 0.4, 0.4))]
            } else {
                column![
                    text("Active Sprites").size(mf),
                    Column::with_children(spr_rows).spacing(4),
                ]
            }
            .spacing(6),
        )
        .style(section_style)
        .padding(8)
        .width(Length::Fill);

        // IRQ / interrupt state
        let irq_row = container(
            row![
                kv("IRQ $D019", &format!("${:02X}", vic.byte(0x19)), mf),
                Space::new().width(16),
                kv("IRQ Mask", &format!("${:02X}", vic.byte(0x1A)), mf),
            ]
            .spacing(8)
            .align_y(iced::Alignment::Center),
        )
        .style(section_style)
        .padding(8)
        .width(Length::Fill);

        scrollable(
            column![
                text("VIC-II @ $D000").size(sf),
                rule::horizontal(1),
                screen_row,
                memory_row,
                colour_row,
                sprites_block,
                irq_row,
            ]
            .spacing(8)
            .width(Length::Fill),
        )
        .height(Length::Fill)
        .into()
    }

    // ── CIA panel ────────────────────────────────────────────────

    fn view_cia(&self, sf: u32) -> Element<'_, SidMonitorMessage> {
        let mf = sf.saturating_sub(1);

        let (Some(cia1), Some(cia2)) = (&self.cia1, &self.cia2) else {
            return self.view_placeholder("No CIA data yet — press ▶ Live", sf);
        };

        scrollable(
            column![
                text("CIA Timers & I/O").size(sf),
                rule::horizontal(1),
                cia_block_view(cia1, "CIA #1 ($DC00) — Keyboard / Joystick / IRQ", sf, mf),
                cia_block_view(cia2, "CIA #2 ($DD00) — Serial / NMI / VIC Bank", sf, mf),
            ]
            .spacing(12)
            .width(Length::Fill),
        )
        .height(Length::Fill)
        .into()
    }

    // ── Placeholder ──────────────────────────────────────────────

    fn view_placeholder<'a>(&self, msg: &'a str, sf: u32) -> Element<'a, SidMonitorMessage> {
        column![
            Space::new().height(Length::Fill),
            text(msg)
                .size(sf)
                .color(iced::Color::from_rgb(0.5, 0.5, 0.5)),
            Space::new().height(Length::Fill),
        ]
        .align_x(iced::Alignment::Center)
        .width(Length::Fill)
        .into()
    }
}

/// Render one CIA chip block. Free function to avoid closure lifetime issues.
fn cia_block_view(
    cia: &CiaState,
    label: &str,
    sf: u32,
    mf: u32,
) -> Element<'static, SidMonitorMessage> {
    let irq_sources: Vec<&str> = [
        (cia.irq_ta(), "TimerA"),
        (cia.irq_tb(), "TimerB"),
        (cia.irq_tod(), "TOD"),
        (cia.irq_sp(), "SerPort"),
        (cia.irq_flag(), "FLAG"),
    ]
    .iter()
    .filter_map(|(active, name)| if *active { Some(*name) } else { None })
    .collect();

    let label = label.to_string(); // own the string to satisfy 'static

    container(
        column![
            text(label).size(sf),
            rule::horizontal(1),
            row![
                kv(
                    "Timer A",
                    &format!(
                        "${:04X}  {}",
                        cia.timer_a(),
                        if cia.timer_a_running() {
                            "▶ RUN"
                        } else {
                            "■ STOP"
                        }
                    ),
                    mf
                ),
                Space::new().width(16),
                kv("CRA", &format!("${:02X}", cia.cra()), mf),
            ]
            .spacing(8),
            row![
                kv(
                    "Timer B",
                    &format!(
                        "${:04X}  {}",
                        cia.timer_b(),
                        if cia.timer_b_running() {
                            "▶ RUN"
                        } else {
                            "■ STOP"
                        }
                    ),
                    mf
                ),
                Space::new().width(16),
                kv("CRB", &format!("${:02X}", cia.crb()), mf),
            ]
            .spacing(8),
            kv("TOD", &cia.tod_string(), mf),
            kv(
                "ICR",
                &format!(
                    "${:02X}  IRQ:{}",
                    cia.icr(),
                    if cia.irq_set() {
                        if irq_sources.is_empty() {
                            "SET".into()
                        } else {
                            irq_sources.join("+")
                        }
                    } else {
                        "clear".into()
                    }
                ),
                mf
            ),
            row![
                kv("Port A", &format!("${:02X}", cia.byte(0x00)), mf),
                Space::new().width(16),
                kv("Dir A", &format!("${:02X}", cia.byte(0x02)), mf),
                Space::new().width(16),
                kv("Port B", &format!("${:02X}", cia.byte(0x01)), mf),
                Space::new().width(16),
                kv("Dir B", &format!("${:02X}", cia.byte(0x03)), mf),
            ]
            .spacing(8),
        ]
        .spacing(6)
        .padding(8),
    )
    .style(section_style)
    .width(Length::Fill)
    .into()
}

// ─────────────────────────────────────────────────────────────────
//  Small view helpers
// ─────────────────────────────────────────────────────────────────

/// Key–value pair rendered as "KEY  value"
fn kv<'a>(key: &str, value: &str, font_size: u32) -> Element<'a, SidMonitorMessage> {
    row![
        text(format!("{}: ", key))
            .size(font_size)
            .color(iced::Color::from_rgb(0.6, 0.6, 0.7)),
        text(value.to_string()).size(font_size),
    ]
    .spacing(0)
    .align_y(iced::Alignment::Center)
    .into()
}

fn yn(b: bool) -> String {
    if b {
        "Yes".into()
    } else {
        "No".into()
    }
}

/// Coloured square swatch + colour name
fn colour_swatch<M: Clone + 'static>(index: u8, font_size: u32) -> Element<'static, M> {
    let (r, g, b, name) = PALETTE[index as usize & 0x0F];
    let rgb = iced::Color::from_rgb(r as f32 / 255.0, g as f32 / 255.0, b as f32 / 255.0);
    row![
        container(Space::new().width(14).height(14)).style(move |_: &iced::Theme| {
            container::Style {
                background: Some(iced::Background::Color(rgb)),
                border: iced::Border {
                    color: iced::Color::from_rgb(0.3, 0.3, 0.3),
                    width: 1.0,
                    radius: 2.0.into(),
                },
                ..Default::default()
            }
        }),
        text(format!("{} ({})", name, index)).size(font_size),
    ]
    .spacing(4)
    .align_y(iced::Alignment::Center)
    .into()
}

/// Column header row for the voice table
fn voice_header_row<M: 'static>(font_size: u32) -> Element<'static, M> {
    let dim = iced::Color::from_rgb(0.45, 0.45, 0.55);
    row![
        text("").size(font_size).width(Length::Fixed(24.0)),
        text("Freq")
            .size(font_size)
            .color(dim)
            .width(Length::Fixed(54.0)),
        text("Hz")
            .size(font_size)
            .color(dim)
            .width(Length::Fixed(80.0)),
        text("Note")
            .size(font_size)
            .color(dim)
            .width(Length::Fixed(36.0)),
        text("Waveform")
            .size(font_size)
            .color(dim)
            .width(Length::Fixed(90.0)),
        text("PulseW")
            .size(font_size)
            .color(dim)
            .width(Length::Fixed(72.0)),
        text("ADSR")
            .size(font_size)
            .color(dim)
            .width(Length::Fixed(200.0)),
        text("Gate")
            .size(font_size)
            .color(dim)
            .width(Length::Fixed(52.0)),
        text("Mods").size(font_size).color(dim),
    ]
    .spacing(6)
    .into()
}

fn section_style(theme: &iced::Theme) -> container::Style {
    crate::styles::section_style(theme)
}

// ─────────────────────────────────────────────────────────────────
//  Async REST helper
// ─────────────────────────────────────────────────────────────────

async fn read_bytes(
    connection: Arc<TokioMutex<Rest>>,
    address: u16,
    length: u16,
) -> Result<Vec<u8>, String> {
    let result = tokio::time::timeout(
        tokio::time::Duration::from_secs(TIMEOUT_SECS),
        tokio::task::spawn_blocking(move || {
            let conn = connection.blocking_lock();
            conn.read_mem(address, length)
                .map_err(|e| format!("read_mem failed: {}", e))
        }),
    )
    .await;
    match result {
        Ok(Ok(data)) => data,
        Ok(Err(e)) => Err(format!("Task error: {}", e)),
        Err(_) => Err("Read timed out — device may be offline".to_string()),
    }
}
