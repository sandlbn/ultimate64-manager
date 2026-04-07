use iced::widget::{button, container};

/// Centralized font size tiers derived from the user's configured base font size.
#[derive(Clone, Copy)]
pub struct FontSizes {
    pub tiny: u32,   // base - 3, min 8 — scale buttons, close "X"
    pub small: u32,  // base - 1, min 8 — buttons, labels, descriptions
    pub normal: u32, // base — body text, standard content
    pub large: u32,  // base + 2 — section labels, nav buttons
    pub header: u32, // base + 6 — section headers
    pub icon: u32,   // base * 2 + 6 — emoji icons
}

impl FontSizes {
    pub fn from_base(font_size: u32) -> Self {
        Self {
            tiny: font_size.saturating_sub(3).max(8),
            small: font_size.saturating_sub(1).max(8),
            normal: font_size,
            large: font_size + 2,
            header: font_size + 6,
            icon: font_size * 2 + 6,
        }
    }
}

/// Highlight style (yellow background, black text) for selected hex cells
pub fn highlight_style(_theme: &iced::Theme) -> container::Style {
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

/// Editing style (green background, black text) for actively edited cells
pub fn editing_style(_theme: &iced::Theme) -> container::Style {
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

/// Tooltip style (dark background with border)
pub fn tooltip_style(_theme: &iced::Theme) -> container::Style {
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

/// Active pane style — gray border to indicate which pane is active
pub fn active_pane_style(_theme: &iced::Theme) -> container::Style {
    container::Style {
        background: None,
        border: iced::Border {
            color: iced::Color::from_rgb(0.55, 0.55, 0.58),
            width: 2.0,
            radius: 0.0.into(),
        },
        ..Default::default()
    }
}

/// Inactive pane style — subtle border
pub fn inactive_pane_style(_theme: &iced::Theme) -> container::Style {
    container::Style {
        background: None,
        border: iced::Border {
            color: iced::Color::from_rgba(1.0, 1.0, 1.0, 0.08),
            width: 1.0,
            radius: 0.0.into(),
        },
        ..Default::default()
    }
}

/// Flat nav button — subtle background, no heavy border. For toolbar/nav buttons.
pub fn nav_button(_theme: &iced::Theme, status: button::Status) -> button::Style {
    let base = button::Style {
        background: Some(iced::Background::Color(iced::Color::from_rgba(
            1.0, 1.0, 1.0, 0.08,
        ))),
        text_color: iced::Color::from_rgb(0.78, 0.78, 0.82),
        border: iced::Border {
            color: iced::Color::from_rgba(1.0, 1.0, 1.0, 0.12),
            width: 1.0,
            radius: 3.0.into(),
        },
        shadow: iced::Shadow::default(),
        snap: false,
    };
    match status {
        button::Status::Hovered => button::Style {
            background: Some(iced::Background::Color(iced::Color::from_rgba(
                1.0, 1.0, 1.0, 0.15,
            ))),
            text_color: iced::Color::WHITE,
            ..base
        },
        button::Status::Pressed => button::Style {
            background: Some(iced::Background::Color(iced::Color::from_rgba(
                1.0, 1.0, 1.0, 0.20,
            ))),
            ..base
        },
        _ => base,
    }
}

/// Action button — slightly more visible, for Run/Mount/Play buttons on file rows.
pub fn action_button(_theme: &iced::Theme, status: button::Status) -> button::Style {
    let base = button::Style {
        background: Some(iced::Background::Color(iced::Color::from_rgba(
            0.55, 0.6, 0.75, 0.25,
        ))),
        text_color: iced::Color::from_rgb(0.8, 0.82, 0.9),
        border: iced::Border {
            color: iced::Color::from_rgba(0.6, 0.65, 0.8, 0.3),
            width: 1.0,
            radius: 3.0.into(),
        },
        shadow: iced::Shadow::default(),
        snap: false,
    };
    match status {
        button::Status::Hovered => button::Style {
            background: Some(iced::Background::Color(iced::Color::from_rgba(
                0.55, 0.6, 0.75, 0.4,
            ))),
            text_color: iced::Color::WHITE,
            ..base
        },
        button::Status::Pressed => button::Style {
            background: Some(iced::Background::Color(iced::Color::from_rgba(
                0.55, 0.6, 0.75, 0.5,
            ))),
            ..base
        },
        _ => base,
    }
}

/// Subtle tooltip container — dark, blends with dark theme
pub fn subtle_tooltip(_theme: &iced::Theme) -> container::Style {
    container::Style {
        background: Some(iced::Background::Color(iced::Color::from_rgb(
            0.15, 0.15, 0.18,
        ))),
        border: iced::Border {
            color: iced::Color::from_rgba(1.0, 1.0, 1.0, 0.15),
            width: 1.0,
            radius: 4.0.into(),
        },
        text_color: Some(iced::Color::from_rgb(0.85, 0.85, 0.88)),
        shadow: iced::Shadow::default(),
        snap: false,
    }
}

/// Section style (subtle bordered container) for grouping related UI elements
pub fn section_style(_theme: &iced::Theme) -> container::Style {
    container::Style {
        background: Some(iced::Background::Color(iced::Color::from_rgba(
            1.0, 1.0, 1.0, 0.04,
        ))),
        border: iced::Border {
            color: iced::Color::from_rgba(1.0, 1.0, 1.0, 0.10),
            width: 1.0,
            radius: 4.0.into(),
        },
        ..Default::default()
    }
}
