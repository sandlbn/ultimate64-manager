use iced::{
    Command, Element, Length,
    widget::{
        Column, Space, button, column, container, horizontal_rule, pick_list, row, scrollable,
        slider, text, text_input, toggler, vertical_rule,
    },
};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::Mutex;
use ultimate64::Rest;

/// A single configuration item with full details (from /v1/configs/<category>/<item>)
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ConfigItemDetails {
    pub current: serde_json::Value,
    #[serde(default)]
    pub min: Option<i64>,
    #[serde(default)]
    pub max: Option<i64>,
    #[serde(default)]
    pub format: Option<String>,
    #[serde(default)]
    pub default: Option<serde_json::Value>,
    #[serde(default)]
    pub options: Option<Vec<String>>,
}

/// Parsed configuration option for UI display
#[derive(Debug, Clone)]
pub struct ConfigOption {
    pub category: String,
    pub name: String,
    pub current_value: serde_json::Value,
    pub details: Option<ConfigItemDetails>,
    pub option_type: ConfigOptionType,
}

#[derive(Debug, Clone, PartialEq)]
pub enum ConfigOptionType {
    Integer,
    String,
    Enum,
    Bool,
    Unknown,
}

#[derive(Debug, Clone)]
pub enum ConfigEditorMessage {
    // Loading
    LoadCategories,
    CategoriesLoaded(Result<Vec<String>, String>),
    SelectCategory(String),
    CategoryItemsLoaded(Result<(String, Vec<ConfigOption>), String>),

    // Load item details (for getting min/max/options)
    LoadItemDetails(String, String),
    ItemDetailsLoaded(Result<(String, ConfigItemDetails), String>),

    // Editing values
    StringValueChanged(String, String, String),
    EnumValueChanged(String, String, String),
    IntValueChanged(String, String, i64),
    BoolValueChanged(String, String, bool),

    // Batch operations
    SaveAllChanges,
    SaveComplete(Result<String, String>),

    // Flash operations
    SaveToFlash,
    LoadFromFlash,
    ResetToDefault,
    FlashOperationComplete(Result<String, String>),

    // Other
    RevertChanges,
    RefreshCategory,
    SearchChanged(String),
}

pub struct ConfigEditor {
    // Categories
    categories: Vec<String>,
    selected_category: Option<String>,

    // Current category items (item_name -> ConfigOption)
    current_items: HashMap<String, ConfigOption>,
    original_values: HashMap<String, serde_json::Value>,

    // Pending changes (category -> item_name -> new_value)
    pending_changes: HashMap<String, HashMap<String, serde_json::Value>>,

    // UI state
    is_loading: bool,
    has_unsaved_changes: bool,
    status_message: Option<String>,
    error_message: Option<String>,
    search_filter: String,
}

impl ConfigEditor {
    pub fn new() -> Self {
        Self {
            categories: Vec::new(),
            selected_category: None,
            current_items: HashMap::new(),
            original_values: HashMap::new(),
            pending_changes: HashMap::new(),
            is_loading: false,
            has_unsaved_changes: false,
            status_message: Some("Click 'Load' to fetch configuration".to_string()),
            error_message: None,
            search_filter: String::new(),
        }
    }

    pub fn update(
        &mut self,
        message: ConfigEditorMessage,
        _connection: Option<Arc<Mutex<Rest>>>,
        host_url: Option<String>,
        password: Option<String>,
    ) -> Command<ConfigEditorMessage> {
        match message {
            ConfigEditorMessage::LoadCategories => {
                if let Some(host) = host_url {
                    self.is_loading = true;
                    self.status_message = Some("Loading categories...".to_string());
                    self.error_message = None;
                    Command::perform(
                        fetch_categories(host, password),
                        ConfigEditorMessage::CategoriesLoaded,
                    )
                } else {
                    self.error_message = Some("Not connected to Ultimate64".to_string());
                    Command::none()
                }
            }

            ConfigEditorMessage::CategoriesLoaded(result) => {
                self.is_loading = false;
                match result {
                    Ok(categories) => {
                        log::info!(
                            "UI received {} categories: {:?}",
                            categories.len(),
                            categories
                        );
                        self.categories = categories;
                        self.status_message =
                            Some(format!("{} categories loaded", self.categories.len()));
                        self.error_message = None;
                    }
                    Err(e) => {
                        log::error!("Failed to load categories: {}", e);
                        self.error_message = Some(format!("Failed to load: {}", e));
                        self.status_message = None;
                    }
                }
                Command::none()
            }

            ConfigEditorMessage::SelectCategory(category) => {
                log::info!("Selecting category: {}", category);
                self.selected_category = Some(category.clone());
                self.search_filter.clear();
                self.current_items.clear();
                self.original_values.clear();

                if let Some(host) = host_url {
                    self.is_loading = true;
                    self.status_message = Some(format!("Loading {}...", category));
                    self.error_message = None;
                    Command::perform(
                        fetch_category_items(host, category, password),
                        ConfigEditorMessage::CategoryItemsLoaded,
                    )
                } else {
                    self.error_message = Some("Not connected".to_string());
                    Command::none()
                }
            }

            ConfigEditorMessage::CategoryItemsLoaded(result) => {
                self.is_loading = false;
                match result {
                    Ok((category, items)) => {
                        log::info!("Loaded {} items for category '{}'", items.len(), category);
                        self.current_items.clear();
                        self.original_values.clear();

                        for item in items {
                            self.original_values
                                .insert(item.name.clone(), item.current_value.clone());
                            self.current_items.insert(item.name.clone(), item);
                        }

                        self.status_message = Some(format!(
                            "{} items in {}",
                            self.current_items.len(),
                            category
                        ));
                        self.error_message = None;
                        // All details are now fetched in one request using wildcard API
                    }
                    Err(e) => {
                        log::error!("Failed to load category items: {}", e);
                        self.error_message = Some(format!("Failed to load: {}", e));
                        self.current_items.clear();
                        self.original_values.clear();
                    }
                }
                Command::none()
            }

            ConfigEditorMessage::LoadItemDetails(category, item_name) => {
                if let Some(host) = host_url {
                    Command::perform(
                        fetch_item_details(host, category, item_name, password),
                        ConfigEditorMessage::ItemDetailsLoaded,
                    )
                } else {
                    Command::none()
                }
            }

            ConfigEditorMessage::ItemDetailsLoaded(result) => {
                if let Ok((item_name, details)) = result {
                    if let Some(item) = self.current_items.get_mut(&item_name) {
                        // Determine type based on details
                        if details.options.is_some()
                            && !details.options.as_ref().unwrap().is_empty()
                        {
                            // Has values/options list -> Enum (dropdown)
                            item.option_type = ConfigOptionType::Enum;
                            log::debug!(
                                "Item '{}' is Enum with {} options",
                                item_name,
                                details.options.as_ref().unwrap().len()
                            );
                        } else if details.min.is_some() || details.max.is_some() {
                            // Has min/max -> Integer (slider)
                            item.option_type = ConfigOptionType::Integer;
                            log::debug!(
                                "Item '{}' is Integer [{:?} - {:?}]",
                                item_name,
                                details.min,
                                details.max
                            );
                        }
                        // Otherwise keep existing type (Bool or String)
                        item.details = Some(details);
                    }
                }
                Command::none()
            }

            ConfigEditorMessage::StringValueChanged(category, name, value) => {
                self.record_change(&category, &name, serde_json::Value::String(value.clone()));
                if let Some(opt) = self.current_items.get_mut(&name) {
                    opt.current_value = serde_json::Value::String(value);
                }
                Command::none()
            }

            ConfigEditorMessage::EnumValueChanged(category, name, value) => {
                self.record_change(&category, &name, serde_json::Value::String(value.clone()));
                if let Some(opt) = self.current_items.get_mut(&name) {
                    opt.current_value = serde_json::Value::String(value);
                }
                Command::none()
            }

            ConfigEditorMessage::IntValueChanged(category, name, value) => {
                self.record_change(&category, &name, serde_json::json!(value));
                if let Some(opt) = self.current_items.get_mut(&name) {
                    opt.current_value = serde_json::json!(value);
                }
                Command::none()
            }

            ConfigEditorMessage::BoolValueChanged(category, name, value) => {
                let str_value = if value { "Yes" } else { "No" };
                self.record_change(
                    &category,
                    &name,
                    serde_json::Value::String(str_value.to_string()),
                );
                if let Some(opt) = self.current_items.get_mut(&name) {
                    opt.current_value = serde_json::Value::String(str_value.to_string());
                }
                Command::none()
            }

            ConfigEditorMessage::SaveAllChanges => {
                if let Some(host) = host_url {
                    if self.pending_changes.is_empty() {
                        self.status_message = Some("No changes to save".to_string());
                        return Command::none();
                    }

                    self.is_loading = true;
                    self.status_message = Some("Saving changes...".to_string());

                    let changes = self.pending_changes.clone();
                    Command::perform(
                        save_batch_changes(host, changes, password),
                        ConfigEditorMessage::SaveComplete,
                    )
                } else {
                    self.error_message = Some("Not connected".to_string());
                    Command::none()
                }
            }

            ConfigEditorMessage::SaveComplete(result) => {
                self.is_loading = false;
                match result {
                    Ok(msg) => {
                        self.status_message = Some(msg);
                        self.error_message = None;
                        for (name, opt) in &self.current_items {
                            self.original_values
                                .insert(name.clone(), opt.current_value.clone());
                        }
                        self.pending_changes.clear();
                        self.has_unsaved_changes = false;
                    }
                    Err(e) => {
                        self.error_message = Some(format!("Save failed: {}", e));
                    }
                }
                Command::none()
            }

            ConfigEditorMessage::SaveToFlash => {
                if let Some(host) = host_url {
                    self.is_loading = true;
                    self.status_message = Some("Saving to flash...".to_string());
                    Command::perform(
                        flash_operation(host, "save_to_flash", password),
                        ConfigEditorMessage::FlashOperationComplete,
                    )
                } else {
                    self.error_message = Some("Not connected".to_string());
                    Command::none()
                }
            }

            ConfigEditorMessage::LoadFromFlash => {
                if let Some(host) = host_url.clone() {
                    self.is_loading = true;
                    self.status_message = Some("Loading from flash...".to_string());
                    Command::perform(
                        flash_operation(host, "load_from_flash", password),
                        ConfigEditorMessage::FlashOperationComplete,
                    )
                } else {
                    self.error_message = Some("Not connected".to_string());
                    Command::none()
                }
            }

            ConfigEditorMessage::ResetToDefault => {
                if let Some(host) = host_url {
                    self.is_loading = true;
                    self.status_message = Some("Resetting to defaults...".to_string());
                    Command::perform(
                        flash_operation(host, "reset_to_default", password),
                        ConfigEditorMessage::FlashOperationComplete,
                    )
                } else {
                    self.error_message = Some("Not connected".to_string());
                    Command::none()
                }
            }

            ConfigEditorMessage::FlashOperationComplete(result) => {
                self.is_loading = false;
                match result {
                    Ok(msg) => {
                        self.status_message = Some(msg);
                        self.error_message = None;
                        self.pending_changes.clear();
                        self.has_unsaved_changes = false;
                    }
                    Err(e) => {
                        self.error_message = Some(e);
                    }
                }
                if let Some(category) = self.selected_category.clone() {
                    return self.update(
                        ConfigEditorMessage::SelectCategory(category),
                        _connection,
                        host_url,
                        password,
                    );
                }
                Command::none()
            }

            ConfigEditorMessage::RevertChanges => {
                for (name, orig_value) in &self.original_values {
                    if let Some(opt) = self.current_items.get_mut(name) {
                        opt.current_value = orig_value.clone();
                    }
                }
                self.pending_changes.clear();
                self.has_unsaved_changes = false;
                self.status_message = Some("Changes reverted".to_string());
                Command::none()
            }

            ConfigEditorMessage::RefreshCategory => {
                if let Some(category) = self.selected_category.clone() {
                    self.pending_changes.clear();
                    self.has_unsaved_changes = false;
                    return self.update(
                        ConfigEditorMessage::SelectCategory(category),
                        _connection,
                        host_url,
                        password,
                    );
                }
                Command::none()
            }

            ConfigEditorMessage::SearchChanged(filter) => {
                self.search_filter = filter;
                Command::none()
            }
        }
    }

    fn record_change(&mut self, category: &str, name: &str, value: serde_json::Value) {
        self.pending_changes
            .entry(category.to_string())
            .or_insert_with(HashMap::new)
            .insert(name.to_string(), value);
        self.has_unsaved_changes = true;
    }

    fn is_item_modified(&self, category: &str, name: &str) -> bool {
        self.pending_changes
            .get(category)
            .map(|items| items.contains_key(name))
            .unwrap_or(false)
    }

    pub fn view(&self, is_connected: bool) -> Element<'_, ConfigEditorMessage> {
        // === LEFT PANE: Category list ===
        let category_header = container(
            column![
                text("CATEGORIES").size(12),
                row![
                    button(text("Load").size(10))
                        .on_press(ConfigEditorMessage::LoadCategories)
                        .padding([4, 8]),
                ]
                .spacing(5),
            ]
            .spacing(5),
        )
        .padding(10);

        let category_list: Element<'_, ConfigEditorMessage> = if self.categories.is_empty() {
            container(
                text(if is_connected {
                    "Click 'Load' to fetch categories"
                } else {
                    "Connect to Ultimate64 first"
                })
                .size(11),
            )
            .padding(10)
            .into()
        } else {
            let items: Vec<Element<'_, ConfigEditorMessage>> = self
                .categories
                .iter()
                .map(|cat| {
                    let is_selected = self.selected_category.as_ref() == Some(cat);
                    let has_changes = self.pending_changes.contains_key(cat);

                    let label = if has_changes {
                        format!("* {}", cat)
                    } else {
                        cat.clone()
                    };

                    button(text(&label).size(11))
                        .on_press(ConfigEditorMessage::SelectCategory(cat.clone()))
                        .padding([6, 10])
                        .width(Length::Fill)
                        .style(if is_selected {
                            iced::theme::Button::Primary
                        } else {
                            iced::theme::Button::Text
                        })
                        .into()
                })
                .collect();

            scrollable(
                Column::with_children(items)
                    .spacing(2)
                    .padding([5, 15, 5, 5]),
            )
            .height(Length::Fill)
            .into()
        };

        // Flash operations
        let flash_controls = container(
            column![
                horizontal_rule(1),
                text("FLASH MEMORY").size(10),
                button(text("Save to Flash").size(10))
                    .on_press(ConfigEditorMessage::SaveToFlash)
                    .padding([4, 8])
                    .width(Length::Fill),
                button(text("Load from Flash").size(10))
                    .on_press(ConfigEditorMessage::LoadFromFlash)
                    .padding([4, 8])
                    .width(Length::Fill),
                button(text("Reset to Default").size(10))
                    .on_press(ConfigEditorMessage::ResetToDefault)
                    .padding([4, 8])
                    .width(Length::Fill),
            ]
            .spacing(5),
        )
        .padding(10);

        let left_pane = container(
            column![
                category_header,
                horizontal_rule(1),
                category_list,
                flash_controls,
            ]
            .spacing(0)
            .height(Length::Fill),
        )
        .width(Length::Fixed(220.0));

        // === RIGHT PANE: Options editor ===
        let options_header = container(
            column![
                row![
                    text(
                        self.selected_category
                            .as_deref()
                            .unwrap_or("Select a category")
                    )
                    .size(14),
                    Space::with_width(Length::Fill),
                    if self.has_unsaved_changes {
                        text("* Modified").size(10).style(iced::theme::Text::Color(
                            iced::Color::from_rgb(0.9, 0.7, 0.0),
                        ))
                    } else {
                        text("").size(10)
                    },
                ]
                .align_items(iced::Alignment::Center),
                row![
                    button(text("Apply All").size(10))
                        .on_press(ConfigEditorMessage::SaveAllChanges)
                        .padding([4, 10])
                        .style(if self.has_unsaved_changes {
                            iced::theme::Button::Primary
                        } else {
                            iced::theme::Button::Secondary
                        }),
                    button(text("Revert").size(10))
                        .on_press(ConfigEditorMessage::RevertChanges)
                        .padding([4, 8]),
                    button(text("Refresh").size(10))
                        .on_press(ConfigEditorMessage::RefreshCategory)
                        .padding([4, 8]),
                    Space::with_width(10),
                    text_input("Filter...", &self.search_filter)
                        .on_input(ConfigEditorMessage::SearchChanged)
                        .size(11)
                        .width(Length::Fixed(120.0)),
                ]
                .spacing(5),
            ]
            .spacing(5),
        )
        .padding(10);

        let options_list: Element<'_, ConfigEditorMessage> = if self.current_items.is_empty() {
            container(if self.is_loading {
                text("Loading...").size(12)
            } else if self.selected_category.is_some() {
                text("No items in this category").size(12)
            } else {
                text("Select a category from the left").size(12)
            })
            .padding(20)
            .center_x()
            .into()
        } else {
            let mut sorted_items: Vec<_> = self.current_items.values().collect();
            sorted_items.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));

            let filter_lower = self.search_filter.to_lowercase();
            let filtered_items: Vec<_> = sorted_items
                .into_iter()
                .filter(|opt| {
                    filter_lower.is_empty() || opt.name.to_lowercase().contains(&filter_lower)
                })
                .collect();

            let items: Vec<Element<'_, ConfigEditorMessage>> = filtered_items
                .iter()
                .map(|opt| self.view_option(opt))
                .collect();

            scrollable(
                Column::with_children(items)
                    .spacing(8)
                    .padding([10, 15, 10, 10]),
            )
            .height(Length::Fill)
            .into()
        };

        let right_pane = container(
            column![options_header, horizontal_rule(1), options_list]
                .spacing(0)
                .height(Length::Fill),
        )
        .width(Length::Fill);

        // === BOTTOM: Status bar ===
        let pending_count: usize = self.pending_changes.values().map(|v| v.len()).sum();

        let status_bar = container(
            row![
                if let Some(err) = &self.error_message {
                    text(err)
                        .size(11)
                        .style(iced::theme::Text::Color(iced::Color::from_rgb(
                            0.9, 0.3, 0.3,
                        )))
                } else if let Some(status) = &self.status_message {
                    text(status).size(11)
                } else {
                    text("").size(11)
                },
                Space::with_width(Length::Fill),
                text(format!("{} items", self.current_items.len())).size(11),
                Space::with_width(10),
                if pending_count > 0 {
                    text(format!("{} pending", pending_count)).size(11)
                } else {
                    text("").size(11)
                },
                Space::with_width(10),
                if self.is_loading {
                    text("Loading...").size(11)
                } else {
                    text("").size(11)
                },
            ]
            .align_items(iced::Alignment::Center),
        )
        .padding([5, 10]);

        column![
            text("CONFIGURATION EDITOR").size(16),
            horizontal_rule(1),
            row![left_pane, vertical_rule(1), right_pane].height(Length::Fill),
            horizontal_rule(1),
            status_bar,
        ]
        .spacing(5)
        .padding(10)
        .into()
    }

    fn view_option(&self, opt: &ConfigOption) -> Element<'_, ConfigEditorMessage> {
        let is_modified = self.is_item_modified(&opt.category, &opt.name);

        let name_row = row![
            text(&opt.name).size(12),
            Space::with_width(5),
            if is_modified {
                text("*")
                    .size(12)
                    .style(iced::theme::Text::Color(iced::Color::from_rgb(
                        0.9, 0.7, 0.0,
                    )))
            } else {
                text("").size(12)
            },
        ]
        .align_items(iced::Alignment::Center);

        let default_text = if let Some(details) = &opt.details {
            if let Some(default) = &details.default {
                text(format!("Default: {}", format_value(default)))
                    .size(10)
                    .style(iced::theme::Text::Color(iced::Color::from_rgb(
                        0.5, 0.5, 0.5,
                    )))
            } else {
                text("").size(10)
            }
        } else {
            text("").size(10)
        };

        let category = opt.category.clone();
        let name = opt.name.clone();

        let control: Element<'_, ConfigEditorMessage> =
            match &opt.option_type {
                ConfigOptionType::Enum => {
                    let options = opt
                        .details
                        .as_ref()
                        .and_then(|d| d.options.clone())
                        .unwrap_or_default();
                    let current_value = opt.current_value.as_str().map(|s| s.to_string());
                    let cat = category.clone();
                    let n = name.clone();

                    if options.is_empty() {
                        let val = current_value.unwrap_or_default();
                        text_input("", &val)
                            .on_input(move |v| {
                                ConfigEditorMessage::StringValueChanged(cat.clone(), n.clone(), v)
                            })
                            .size(11)
                            .width(Length::Fixed(250.0))
                            .into()
                    } else {
                        pick_list(options, current_value, move |v| {
                            ConfigEditorMessage::EnumValueChanged(cat.clone(), n.clone(), v)
                        })
                        .text_size(11)
                        .width(Length::Fixed(250.0))
                        .into()
                    }
                }

                ConfigOptionType::Integer => {
                    let current_value = opt.current_value.as_i64().unwrap_or(0);
                    let (min, max) = if let Some(details) = &opt.details {
                        (details.min.unwrap_or(0), details.max.unwrap_or(100))
                    } else {
                        (0, 100)
                    };
                    let format = opt
                        .details
                        .as_ref()
                        .and_then(|d| d.format.clone())
                        .unwrap_or_else(|| "%d".to_string());
                    let cat = category.clone();
                    let n = name.clone();

                    let unit = if format.contains("dB") {
                        " dB"
                    } else if format.ends_with('%') {
                        "%"
                    } else {
                        ""
                    };

                    row![
                        slider(min as f64..=max as f64, current_value as f64, move |v| {
                            ConfigEditorMessage::IntValueChanged(cat.clone(), n.clone(), v as i64)
                        })
                        .step(1.0)
                        .width(Length::Fixed(150.0)),
                        Space::with_width(10),
                        text(format!("{}{}", current_value, unit)).size(11),
                        Space::with_width(10),
                        text(format!("[{} - {}]", min, max)).size(10).style(
                            iced::theme::Text::Color(iced::Color::from_rgb(0.5, 0.5, 0.5))
                        ),
                    ]
                    .spacing(5)
                    .align_items(iced::Alignment::Center)
                    .into()
                }

                ConfigOptionType::Bool => {
                    let current_str = opt.current_value.as_str().unwrap_or("");
                    let current_value = matches!(
                        current_str.to_lowercase().as_str(),
                        "enabled" | "yes" | "on" | "true" | "1"
                    );
                    let cat = category.clone();
                    let n = name.clone();

                    row![
                        toggler(String::new(), current_value, move |v| {
                            ConfigEditorMessage::BoolValueChanged(cat.clone(), n.clone(), v)
                        })
                        .size(18),
                        Space::with_width(10),
                        text(if current_value { "Yes" } else { "No" }).size(11),
                    ]
                    .spacing(5)
                    .align_items(iced::Alignment::Center)
                    .into()
                }

                ConfigOptionType::String | ConfigOptionType::Unknown => {
                    let current_value = format_value(&opt.current_value);
                    let cat = category.clone();
                    let n = name.clone();

                    text_input("", &current_value)
                        .on_input(move |v| {
                            ConfigEditorMessage::StringValueChanged(cat.clone(), n.clone(), v)
                        })
                        .size(11)
                        .width(Length::Fixed(250.0))
                        .into()
                }
            };

        container(column![name_row, default_text, control,].spacing(3))
            .padding([8, 10])
            .width(Length::Fill)
            .style(iced::theme::Container::Box)
            .into()
    }
}

// === Helper functions ===

fn determine_option_type(value: &serde_json::Value) -> ConfigOptionType {
    if let Some(s) = value.as_str() {
        let lower = s.to_lowercase();
        if matches!(
            lower.as_str(),
            "enabled" | "disabled" | "yes" | "no" | "on" | "off" | "true" | "false"
        ) {
            return ConfigOptionType::Bool;
        }
        return ConfigOptionType::String;
    }

    if value.is_i64() || value.is_u64() || value.is_f64() {
        return ConfigOptionType::Integer;
    }

    if value.is_boolean() {
        return ConfigOptionType::Bool;
    }

    ConfigOptionType::Unknown
}

fn format_value(value: &serde_json::Value) -> String {
    match value {
        serde_json::Value::String(s) => s.clone(),
        serde_json::Value::Number(n) => n.to_string(),
        serde_json::Value::Bool(b) => if *b { "Yes" } else { "No" }.to_string(),
        serde_json::Value::Null => String::new(),
        _ => value.to_string(),
    }
}

// === Async API functions ===

async fn fetch_categories(host: String, password: Option<String>) -> Result<Vec<String>, String> {
    log::info!("Fetching config categories from {}/v1/configs", host);

    let url = format!("{}/v1/configs", host);

    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(10))
        .build()
        .map_err(|e| format!("HTTP client error: {}", e))?;

    let mut request = client.get(&url);

    // Add X-password header if password is configured
    if let Some(ref pwd) = password {
        if !pwd.is_empty() {
            request = request.header("X-password", pwd.as_str());
        }
    }

    let response = request
        .send()
        .await
        .map_err(|e| format!("Request failed: {}", e))?;

    if !response.status().is_success() {
        return Err(format!("HTTP error: {}", response.status()));
    }

    let text = response
        .text()
        .await
        .map_err(|e| format!("Read error: {}", e))?;
    log::info!("Raw categories response: {}", text);

    // Parse as generic JSON first to inspect structure
    let json: serde_json::Value = serde_json::from_str(&text)
        .map_err(|e| format!("JSON parse error: {} - Response: {}", e, text))?;

    log::info!("Parsed JSON: {:?}", json);

    // Try to extract categories array
    let mut categories = Vec::new();

    if let Some(obj) = json.as_object() {
        // Look for "categories" key with array value
        if let Some(cats_value) = obj.get("categories") {
            if let Some(cats_array) = cats_value.as_array() {
                for cat in cats_array {
                    if let Some(cat_str) = cat.as_str() {
                        categories.push(cat_str.to_string());
                    }
                }
                log::info!(
                    "Extracted {} categories from 'categories' array",
                    categories.len()
                );
            } else {
                log::warn!(
                    "'categories' key exists but is not an array: {:?}",
                    cats_value
                );
            }
        } else {
            // Fallback: treat object keys as categories (except "errors")
            log::info!("No 'categories' key found, using object keys as categories");
            for (key, _value) in obj {
                if key != "errors" {
                    categories.push(key.clone());
                }
            }
        }
    } else {
        return Err(format!("Expected JSON object, got: {}", text));
    }

    if categories.is_empty() {
        return Err(format!("No categories found in response: {}", text));
    }

    log::info!(
        "Final categories list ({} items): {:?}",
        categories.len(),
        categories
    );
    Ok(categories)
}

async fn fetch_category_items(
    host: String,
    category: String,
    password: Option<String>,
) -> Result<(String, Vec<ConfigOption>), String> {
    log::info!("Fetching config items for category: {}", category);

    // Use wildcard to get ALL items with FULL details in one request: /v1/configs/<category>/*
    let encoded_category = urlencoding::encode(&category);
    let url = format!("{}/v1/configs/{}/*", host, encoded_category);

    log::info!("Request URL: {}", url);

    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(15))
        .build()
        .map_err(|e| format!("HTTP client error: {}", e))?;

    let mut request = client.get(&url);

    if let Some(ref pwd) = password {
        if !pwd.is_empty() {
            request = request.header("X-password", pwd.as_str());
        }
    }

    let response = request
        .send()
        .await
        .map_err(|e| format!("Request failed: {}", e))?;

    if !response.status().is_success() {
        return Err(format!("HTTP error: {}", response.status()));
    }

    let text = response
        .text()
        .await
        .map_err(|e| format!("Read error: {}", e))?;
    log::info!("Category items response length: {} bytes", text.len());
    log::debug!("Category items response for '{}': {}", category, text);

    let json: serde_json::Value =
        serde_json::from_str(&text).map_err(|e| format!("JSON parse error: {}", e))?;

    let mut items = Vec::new();

    // Response format with wildcard:
    // { "Category Name": { "Item1": { "current": ..., "values": [...], "default": ... }, ... }, "errors": [] }
    if let Some(obj) = json.as_object() {
        for (cat_name, cat_value) in obj {
            if cat_name == "errors" {
                continue;
            }

            if let Some(cat_obj) = cat_value.as_object() {
                for (item_name, item_value) in cat_obj {
                    if let Some(item_obj) = item_value.as_object() {
                        // Parse full item details
                        let current = item_obj
                            .get("current")
                            .cloned()
                            .unwrap_or(serde_json::Value::Null);

                        // Check for "options" or "values" (API uses "values" for enums)
                        let options = item_obj
                            .get("options")
                            .or_else(|| item_obj.get("values"))
                            .and_then(|v| {
                                v.as_array().map(|arr| {
                                    arr.iter()
                                        .filter_map(|v| v.as_str().map(|s| s.to_string()))
                                        .collect()
                                })
                            });

                        let details = ConfigItemDetails {
                            current: current.clone(),
                            min: item_obj.get("min").and_then(|v| v.as_i64()),
                            max: item_obj.get("max").and_then(|v| v.as_i64()),
                            format: item_obj
                                .get("format")
                                .and_then(|v| v.as_str())
                                .map(|s| s.to_string()),
                            default: item_obj.get("default").cloned(),
                            options,
                        };

                        // Determine option type from details
                        let option_type = if details.options.is_some()
                            && !details.options.as_ref().unwrap().is_empty()
                        {
                            ConfigOptionType::Enum
                        } else if details.min.is_some() || details.max.is_some() {
                            ConfigOptionType::Integer
                        } else {
                            determine_option_type(&current)
                        };

                        log::debug!(
                            "Item '{}': type={:?}, options={:?}, min={:?}, max={:?}",
                            item_name,
                            option_type,
                            details.options.as_ref().map(|o| o.len()),
                            details.min,
                            details.max
                        );

                        items.push(ConfigOption {
                            category: cat_name.clone(),
                            name: item_name.clone(),
                            current_value: current,
                            details: Some(details),
                            option_type,
                        });
                    }
                }
            }
        }
    }

    items.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));

    log::info!(
        "Loaded {} items for category '{}' (with full details)",
        items.len(),
        category
    );
    Ok((category, items))
}

async fn fetch_item_details(
    host: String,
    category: String,
    item_name: String,
    password: Option<String>,
) -> Result<(String, ConfigItemDetails), String> {
    log::debug!("Fetching details for {}/{}", category, item_name);

    // GET /v1/configs/<category>/<item> returns full details with min/max/options
    let encoded_category = urlencoding::encode(&category);
    let encoded_item = urlencoding::encode(&item_name);
    let url = format!("{}/v1/configs/{}/{}", host, encoded_category, encoded_item);

    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(5))
        .build()
        .map_err(|e| format!("HTTP client error: {}", e))?;

    let mut request = client.get(&url);

    if let Some(ref pwd) = password {
        if !pwd.is_empty() {
            request = request.header("X-password", pwd.as_str());
        }
    }

    let response = request
        .send()
        .await
        .map_err(|e| format!("Request failed: {}", e))?;

    if !response.status().is_success() {
        return Err(format!("HTTP error: {}", response.status()));
    }

    let text = response
        .text()
        .await
        .map_err(|e| format!("Read error: {}", e))?;
    log::debug!("Item details response for {}: {}", item_name, text);

    let json: serde_json::Value =
        serde_json::from_str(&text).map_err(|e| format!("JSON parse error: {}", e))?;

    // Response format: { "Category Name": { "Item Name": { "current": ..., "min": ..., "max": ..., ... } } }
    if let Some(obj) = json.as_object() {
        for (_cat_name, cat_value) in obj {
            if let Some(cat_obj) = cat_value.as_object() {
                for (_item_key, item_value) in cat_obj {
                    if let Some(item_obj) = item_value.as_object() {
                        // Check for "options" or "values" (API uses "values" for enums)
                        let options = item_obj
                            .get("options")
                            .or_else(|| item_obj.get("values"))
                            .and_then(|v| {
                                v.as_array().map(|arr| {
                                    arr.iter()
                                        .filter_map(|v| v.as_str().map(|s| s.to_string()))
                                        .collect()
                                })
                            });

                        let details = ConfigItemDetails {
                            current: item_obj
                                .get("current")
                                .cloned()
                                .unwrap_or(serde_json::Value::Null),
                            min: item_obj.get("min").and_then(|v| v.as_i64()),
                            max: item_obj.get("max").and_then(|v| v.as_i64()),
                            format: item_obj
                                .get("format")
                                .and_then(|v| v.as_str())
                                .map(|s| s.to_string()),
                            default: item_obj.get("default").cloned(),
                            options,
                        };

                        log::debug!(
                            "Item details for '{}': min={:?}, max={:?}, options={:?}",
                            item_name,
                            details.min,
                            details.max,
                            details.options.as_ref().map(|o| o.len())
                        );

                        return Ok((item_name, details));
                    }
                }
            }
        }
    }

    Err("Item details not found in response".to_string())
}

async fn save_batch_changes(
    host: String,
    changes: HashMap<String, HashMap<String, serde_json::Value>>,
    password: Option<String>,
) -> Result<String, String> {
    log::info!("Saving batch config changes: {:?}", changes);

    let url = format!("{}/v1/configs", host);

    let mut body = serde_json::Map::new();

    for (category, items) in &changes {
        let mut category_obj = serde_json::Map::new();
        for (item_name, value) in items {
            let raw_value = match value {
                serde_json::Value::String(s) => serde_json::Value::String(s.clone()),
                serde_json::Value::Number(n) => serde_json::Value::Number(n.clone()),
                serde_json::Value::Bool(b) => serde_json::Value::Bool(*b),
                _ => value.clone(),
            };
            category_obj.insert(item_name.clone(), raw_value);
        }
        body.insert(category.clone(), serde_json::Value::Object(category_obj));
    }

    log::info!(
        "POST body: {}",
        serde_json::to_string_pretty(&body).unwrap_or_default()
    );

    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(10))
        .build()
        .map_err(|e| format!("HTTP client error: {}", e))?;

    let mut request = client.post(&url).header("Content-Type", "application/json");

    if let Some(ref pwd) = password {
        if !pwd.is_empty() {
            request = request.header("X-password", pwd.as_str());
        }
    }

    let response = request
        .json(&serde_json::Value::Object(body))
        .send()
        .await
        .map_err(|e| format!("Request failed: {}", e))?;

    let status = response.status();
    let response_text = response.text().await.unwrap_or_default();

    if status.is_success() {
        let total_items: usize = changes.values().map(|v| v.len()).sum();
        Ok(format!("Saved {} setting(s)", total_items))
    } else {
        Err(format!("Save failed: {} - {}", status, response_text))
    }
}

async fn flash_operation(
    host: String,
    operation: &'static str,
    password: Option<String>,
) -> Result<String, String> {
    log::info!("Flash operation: {}", operation);

    let url = format!("{}/v1/configs:{}", host, operation);

    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .build()
        .map_err(|e| format!("HTTP client error: {}", e))?;

    let mut request = client.put(&url);

    if let Some(ref pwd) = password {
        if !pwd.is_empty() {
            request = request.header("X-password", pwd.as_str());
        }
    }

    let response = request
        .send()
        .await
        .map_err(|e| format!("Request failed: {}", e))?;

    if response.status().is_success() {
        match operation {
            "save_to_flash" => Ok("Configuration saved to flash memory".to_string()),
            "load_from_flash" => Ok("Configuration loaded from flash memory".to_string()),
            "reset_to_default" => Ok("Configuration reset to factory defaults".to_string()),
            _ => Ok("Operation completed".to_string()),
        }
    } else {
        let status = response.status();
        let text = response.text().await.unwrap_or_default();
        Err(format!("Flash operation failed: {} - {}", status, text))
    }
}
