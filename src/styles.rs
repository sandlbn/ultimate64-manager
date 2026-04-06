use iced::widget::container;

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
