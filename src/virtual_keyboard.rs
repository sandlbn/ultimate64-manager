//! On-screen C64 keyboard. Clicking a key injects the corresponding PETSCII
//! byte into the device's keyboard buffer (see `streaming::send_petscii`), so the
//! user can enter any PETSCII character — including the SHIFT/C= graphics that a
//! PC keyboard can't type. Key glyphs are drawn from the embedded C64 character
//! ROM ([`crate::screenshot_api::embedded_char_rom`]), so they render correctly
//! regardless of the host font.

use iced::widget::image::Handle;
use iced::widget::{button, column, container, image as iced_image, row, text, Space};
use iced::{Alignment, Element, Length};

use crate::streaming::StreamingMessage;

/// Modifier ids carried by `StreamingMessage::VkModifier`.
pub const MOD_SHIFT: u8 = 0;
pub const MOD_COMMODORE: u8 = 1;
pub const MOD_CTRL: u8 = 2;

/// Convert a PETSCII code to its screen (character-ROM) code so we can look up
/// the glyph. Standard C64 mapping.
fn petscii_to_screencode(c: u8) -> u8 {
    match c {
        0..=31 => c + 128,
        32..=63 => c,
        64..=95 => c - 64,
        96..=127 => c - 32,
        128..=159 => c + 64,
        160..=191 => c - 64,
        192..=223 => c - 128,
        224..=254 => c - 128,
        255 => 94,
    }
}

/// Pre-render the 128 uppercase/graphics glyphs (screen codes 0-127) as small
/// white-on-transparent images. Built once when the keyboard is first shown.
pub fn build_glyphs() -> Vec<Handle> {
    const SCALE: usize = 3;
    const SIZE: usize = 8 * SCALE;
    let rom = crate::screenshot_api::embedded_char_rom();
    let mut out = Vec::with_capacity(128);
    for sc in 0..128usize {
        let mut rgba = vec![0u8; SIZE * SIZE * 4];
        for py in 0..8 {
            let byte = rom[sc * 8 + py];
            for px in 0..8 {
                if byte & (0x80 >> px) != 0 {
                    for dy in 0..SCALE {
                        for dx in 0..SCALE {
                            let x = px * SCALE + dx;
                            let y = py * SCALE + dy;
                            let i = (y * SIZE + x) * 4;
                            rgba[i] = 235;
                            rgba[i + 1] = 235;
                            rgba[i + 2] = 240;
                            rgba[i + 3] = 255;
                        }
                    }
                }
            }
        }
        out.push(Handle::from_rgba(SIZE as u32, SIZE as u32, rgba));
    }
    out
}

/// One key. `Ch` renders its glyph (which changes with the active modifiers);
/// `Special` is a labelled key; `Mod` toggles a modifier; `Gap` is spacing.
enum Key {
    Ch {
        normal: u8,
        shift: u8,
        comm: u8,
    },
    Special {
        label: &'static str,
        code: u8,
        shift_code: u8,
        width: f32,
    },
    Mod {
        label: &'static str,
        id: u8,
        width: f32,
    },
    Gap(f32),
}

fn ch(normal: u8, shift: u8, comm: u8) -> Key {
    Key::Ch {
        normal,
        shift,
        comm,
    }
}

/// The C64 keyboard layout, row by row.
fn layout() -> Vec<Vec<Key>> {
    let sp = |label, code, shift_code, width| Key::Special {
        label,
        code,
        shift_code,
        width,
    };
    vec![
        vec![
            ch(95, 95, 95), // <- left arrow
            ch(49, 33, 49),
            ch(50, 34, 50),
            ch(51, 35, 51),
            ch(52, 36, 52),
            ch(53, 37, 53),
            ch(54, 38, 54),
            ch(55, 39, 55),
            ch(56, 40, 56),
            ch(57, 41, 57),
            ch(48, 48, 48),
            ch(43, 43, 166),  // +
            ch(45, 45, 220),  // -
            ch(92, 169, 168), // £
            sp("CLR", 19, 147, 1.4),
            sp("DEL", 20, 148, 1.4),
        ],
        vec![
            Key::Mod {
                label: "CTRL",
                id: MOD_CTRL,
                width: 1.5,
            },
            ch(81, 209, 171),
            ch(87, 215, 179),
            ch(69, 197, 177),
            ch(82, 210, 178),
            ch(84, 212, 163),
            ch(89, 217, 183),
            ch(85, 213, 184),
            ch(73, 201, 162),
            ch(79, 207, 185),
            ch(80, 208, 175),
            ch(64, 186, 164), // @
            ch(42, 192, 223), // *
            ch(94, 94, 94),   // up arrow
        ],
        vec![
            sp("R/S", 3, 3, 1.5),
            ch(65, 193, 176),
            ch(83, 211, 174),
            ch(68, 196, 172),
            ch(70, 198, 187),
            ch(71, 199, 165),
            ch(72, 200, 180),
            ch(74, 202, 181),
            ch(75, 203, 161),
            ch(76, 204, 182),
            ch(58, 91, 58), // :
            ch(59, 93, 59), // ;
            ch(61, 61, 61), // =
            sp("RETURN", 13, 13, 1.9),
        ],
        vec![
            Key::Mod {
                label: "C=",
                id: MOD_COMMODORE,
                width: 1.4,
            },
            Key::Mod {
                label: "SHIFT",
                id: MOD_SHIFT,
                width: 1.6,
            },
            ch(90, 218, 173),
            ch(88, 216, 189),
            ch(67, 195, 188),
            ch(86, 214, 190),
            ch(66, 194, 191),
            ch(78, 206, 170),
            ch(77, 205, 167),
            ch(44, 60, 44), // ,
            ch(46, 62, 46), // .
            ch(47, 63, 47), // /
            Key::Mod {
                label: "SHIFT",
                id: MOD_SHIFT,
                width: 1.6,
            },
            sp("↕", 17, 145, 1.2),
            sp("↔", 29, 157, 1.2),
        ],
        vec![
            sp("F1", 133, 137, 1.3),
            sp("F3", 134, 138, 1.3),
            sp("F5", 135, 139, 1.3),
            sp("F7", 136, 140, 1.3),
            Key::Gap(0.4),
            sp("SPACE", 32, 32, 7.0),
        ],
    ]
}

/// Resolve a character key to the PETSCII byte it sends under the current
/// modifier state.
fn resolve(normal: u8, shift: u8, comm: u8, sh: bool, cm: bool, ct: bool) -> u8 {
    if ct {
        // CTRL sends the control-range code for letters; otherwise the normal byte.
        if (65..=90).contains(&normal) {
            normal & 0x1F
        } else {
            normal
        }
    } else if cm {
        comm
    } else if sh {
        shift
    } else {
        normal
    }
}

const KEY_UNIT: f32 = 26.0;
const GLYPH_PX: f32 = 18.0;

/// Build the keyboard element. `glyphs` are the pre-rendered screen-code glyphs.
pub fn view<'a>(
    glyphs: &[Handle],
    shift: bool,
    comm: bool,
    ctrl: bool,
    fs: &crate::styles::FontSizes,
) -> Element<'a, StreamingMessage> {
    let mut rows: Vec<Element<'a, StreamingMessage>> = Vec::new();
    for r in layout() {
        let mut cells: Vec<Element<'a, StreamingMessage>> = Vec::new();
        for key in r {
            match key {
                Key::Ch {
                    normal,
                    shift: sh,
                    comm: cm,
                } => {
                    let code = resolve(normal, sh, cm, shift, comm, ctrl);
                    let scr = (petscii_to_screencode(code) & 0x7F) as usize;
                    let glyph: Element<'a, StreamingMessage> = match glyphs.get(scr) {
                        Some(h) => iced_image(h.clone())
                            .width(Length::Fixed(GLYPH_PX))
                            .height(Length::Fixed(GLYPH_PX))
                            .into(),
                        None => Space::new().into(),
                    };
                    cells.push(
                        button(
                            container(glyph)
                                .center_x(Length::Fill)
                                .center_y(Length::Fill),
                        )
                        .width(Length::Fixed(KEY_UNIT))
                        .height(Length::Fixed(KEY_UNIT))
                        .padding(2)
                        .style(button::secondary)
                        .on_press(StreamingMessage::VkSend(code))
                        .into(),
                    );
                }
                Key::Special {
                    label,
                    code,
                    shift_code,
                    width,
                } => {
                    let out = if shift { shift_code } else { code };
                    cells.push(
                        button(text(label).size(fs.tiny))
                            .width(Length::Fixed(KEY_UNIT * width))
                            .height(Length::Fixed(KEY_UNIT))
                            .padding(2)
                            .style(button::secondary)
                            .on_press(StreamingMessage::VkSend(out))
                            .into(),
                    );
                }
                Key::Mod { label, id, width } => {
                    let active = match id {
                        MOD_SHIFT => shift,
                        MOD_COMMODORE => comm,
                        _ => ctrl,
                    };
                    cells.push(
                        button(text(label).size(fs.tiny))
                            .width(Length::Fixed(KEY_UNIT * width))
                            .height(Length::Fixed(KEY_UNIT))
                            .padding(2)
                            .style(if active {
                                button::primary
                            } else {
                                button::secondary
                            })
                            .on_press(StreamingMessage::VkModifier(id))
                            .into(),
                    );
                }
                Key::Gap(w) => {
                    cells.push(Space::new().width(Length::Fixed(KEY_UNIT * w)).into());
                }
            }
        }
        rows.push(row(cells).spacing(3).align_y(Alignment::Center).into());
    }

    container(column(rows).spacing(3))
        .padding(6)
        .style(|_theme| container::Style {
            background: Some(iced::Background::Color(iced::Color::from_rgba(
                0.0, 0.0, 0.0, 0.72,
            ))),
            border: iced::Border {
                radius: 8.0.into(),
                ..Default::default()
            },
            ..Default::default()
        })
        .into()
}
