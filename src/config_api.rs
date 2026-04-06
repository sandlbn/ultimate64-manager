use crate::config_editor::{ConfigItemDetails, ConfigOption, ConfigOptionType};
use crate::config_presets::ConfigPreset;
use std::collections::HashMap;

// === Async API functions ===

pub async fn fetch_categories(
    host: String,
    password: Option<String>,
) -> Result<Vec<String>, String> {
    log::info!("Fetching config categories from {}/v1/configs", host);

    let url = format!("{}/v1/configs", host);

    let client = crate::net_utils::build_device_client(10)?;

    let request = crate::net_utils::with_password(client.get(&url), password.as_deref());

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

pub async fn fetch_category_items(
    host: String,
    category: String,
    password: Option<String>,
) -> Result<(String, Vec<ConfigOption>), String> {
    log::info!("Fetching config items for category: {}", category);

    // Use wildcard to get ALL items with FULL details in one request: /v1/configs/<category>/*
    let encoded_category = urlencoding::encode(&category);
    let url = format!("{}/v1/configs/{}/*", host, encoded_category);

    log::info!("Request URL: {}", url);

    let client = crate::net_utils::build_device_client(15)?;

    let request = crate::net_utils::with_password(client.get(&url), password.as_deref());

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

pub async fn fetch_item_details(
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

    let client = crate::net_utils::build_device_client(5)?;

    let request = crate::net_utils::with_password(client.get(&url), password.as_deref());

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

pub async fn save_batch_changes(
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

    let client = crate::net_utils::build_device_client(10)?;

    let request = crate::net_utils::with_password(
        client.post(&url).header("Content-Type", "application/json"),
        password.as_deref(),
    );

    let response = request
        .json(&serde_json::Value::Object(body.clone()))
        .send()
        .await;

    match response {
        Ok(response) => {
            let status = response.status();
            let response_text = response.text().await.unwrap_or_default();

            if status.is_success() {
                let total_items: usize = changes.values().map(|v| v.len()).sum();
                Ok(format!("Saved {} setting(s)", total_items))
            } else {
                Err(format!("Save failed: {} - {}", status, response_text))
            }
        }
        Err(e) => {
            let err_msg = format!("Request failed: {}", e);
            // Network-related categories cause the device to reconfigure its
            // network stack, which drops the TCP connection. The settings are
            // applied before the connection resets, so treat this as success.
            let has_network_category = changes.keys().any(|c| is_network_related_category(c));
            if has_network_category && is_connection_reset_error(&err_msg) {
                let total_items: usize = changes.values().map(|v| v.len()).sum();
                log::info!(
                    "Connection reset after saving network settings - settings were likely applied"
                );
                Ok(format!(
                    "Saved {} setting(s) (connection reset - normal for network settings)",
                    total_items
                ))
            } else {
                Err(err_msg)
            }
        }
    }
}

pub async fn flash_operation(
    host: String,
    operation: &'static str,
    password: Option<String>,
) -> Result<String, String> {
    log::info!("Flash operation: {}", operation);

    let url = format!("{}/v1/configs:{}", host, operation);

    let client = crate::net_utils::build_device_client(30)?;

    let request = crate::net_utils::with_password(client.put(&url), password.as_deref());

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

pub async fn fetch_all_config(
    host: String,
    categories: Vec<String>,
    password: Option<String>,
) -> Result<ConfigPreset, String> {
    log::info!(
        "Fetching full configuration backup ({} categories)",
        categories.len()
    );

    let mut preset = ConfigPreset::new();
    preset.name = Some("Full Configuration Backup".to_string());
    preset.description = Some(format!(
        "Complete backup of {} categories",
        categories.len()
    ));

    for (i, category) in categories.iter().enumerate() {
        log::info!(
            "Fetching category {}/{}: '{}'",
            i + 1,
            categories.len(),
            category
        );
        match fetch_category_items(host.clone(), category.clone(), password.clone()).await {
            Ok((_cat, items)) => {
                for item in items {
                    preset.add_setting(&item.category, &item.name, item.current_value);
                }
            }
            Err(e) => {
                log::error!("Failed to fetch category '{}': {}", category, e);
                return Err(format!("Failed to fetch category '{}': {}", category, e));
            }
        }
        // Delay after each request to give the device time to recover
        tokio::time::sleep(std::time::Duration::from_millis(1500)).await;
    }

    log::info!(
        "Full config backup complete: {} categories, {} total settings",
        preset.settings.len(),
        preset.setting_count()
    );
    Ok(preset)
}

pub async fn apply_all_config(
    host: String,
    settings: HashMap<String, HashMap<String, serde_json::Value>>,
    password: Option<String>,
) -> Result<String, String> {
    let total_categories = settings.len();
    let mut total_items = 0;

    // Apply one category at a time with delays to avoid overwhelming the device
    let categories: Vec<_> = settings.keys().cloned().collect();
    for (i, category) in categories.iter().enumerate() {
        let items = settings.get(category).unwrap();
        log::info!(
            "Restoring category {}/{}: '{}' ({} settings)",
            i + 1,
            categories.len(),
            category,
            items.len()
        );
        let mut single = HashMap::new();
        single.insert(category.clone(), items.clone());
        total_items += items.len();

        // Retry with increasing backoff to handle transient connection drops
        // (e.g. device reconfiguring network after Ethernet/WiFi settings change)
        let mut last_err = String::new();
        let mut success = false;
        let is_network_category = is_network_related_category(category);
        for attempt in 0..3 {
            if attempt > 0 {
                let backoff = std::time::Duration::from_millis(2000 * (attempt as u64 + 1));
                log::info!(
                    "Retry {}/3 for '{}' after {}ms",
                    attempt + 1,
                    category,
                    backoff.as_millis()
                );
                tokio::time::sleep(backoff).await;
            }
            match save_batch_changes(host.clone(), single.clone(), password.clone()).await {
                Ok(_) => {
                    success = true;
                    break;
                }
                Err(e) => {
                    // Network-related categories cause the device to reconfigure its
                    // network stack, which drops the TCP connection. The settings are
                    // applied before the connection resets, so treat this as success.
                    if is_network_category && is_connection_reset_error(&e) {
                        log::info!(
                            "Connection reset after '{}' - expected for network settings, treating as success",
                            category
                        );
                        success = true;
                        break;
                    }
                    log::warn!("Attempt {} failed for '{}': {}", attempt + 1, category, e);
                    last_err = e;
                }
            }
        }
        if !success {
            return Err(format!(
                "Failed to restore '{}' after 3 attempts: {}",
                category, last_err
            ));
        }

        // Extra delay after network categories to let the device finish reconfiguring
        let delay = if is_network_category { 3000 } else { 1500 };
        tokio::time::sleep(std::time::Duration::from_millis(delay)).await;
    }

    log::info!(
        "Full config restore complete: {} settings across {} categories",
        total_items,
        total_categories
    );
    Ok(format!(
        "Restored {} settings across {} categories",
        total_items, total_categories
    ))
}

// === Helper functions ===

pub fn determine_option_type(value: &serde_json::Value) -> ConfigOptionType {
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

pub fn format_value(value: &serde_json::Value) -> String {
    match value {
        serde_json::Value::String(s) => s.clone(),
        serde_json::Value::Number(n) => n.to_string(),
        serde_json::Value::Bool(b) => if *b { "Yes" } else { "No" }.to_string(),
        serde_json::Value::Null => String::new(),
        _ => value.to_string(),
    }
}

/// Check if a category name relates to network/ethernet/wifi settings.
/// The device may reset its network connection when these are changed.
pub fn is_network_related_category(category: &str) -> bool {
    let lower = category.to_lowercase();
    lower.contains("ethernet") || lower.contains("wifi") || lower.contains("network")
}

/// Check if an error message indicates a connection reset (OS error 54 on macOS, 104 on Linux)
pub fn is_connection_reset_error(err: &str) -> bool {
    let lower = err.to_lowercase();
    lower.contains("connection reset")
        || lower.contains("os error 54")
        || lower.contains("os error 104")
        || lower.contains("broken pipe")
}
