//! CSDB screenshot fallback for Assembly64 entries.
//!
//! Assembly64 doesn't host preview images. For entries whose category came
//! from CSDB (`is_csdb_category(...)`), the Assembly64 `item_id` equals
//! the CSDB release id, so we can pull the screenshot URL out of CSDB's
//! XML webservice and download the image bytes.
//!
//! This is the **only** CSDB call left after the Assembly64 migration —
//! and it's against the stable XML webservice, not the HTML site, so it
//! doesn't share the brittleness of the old scraper.

use anyhow::Result;
use reqwest::Client;

use crate::assembly64::is_csdb_category;

const WEBSERVICE_URL: &str = "https://csdb.dk/webservice/";

/// Fetch the CSDB screenshot for an Assembly64 entry.
///
/// Returns:
/// - `Ok(Some(bytes))` — image bytes (PNG / GIF / JPG, decided by CSDB).
/// - `Ok(None)` — entry isn't in a CSDB-derived category, or CSDB has no
///   screenshot for that release. Either way, **no error** — the UI shows
///   a "no screenshot available" placeholder.
/// - `Err(...)` — actual network or HTTP failure.
pub async fn fetch_screenshot(
    http: &Client,
    item_id: &str,
    category_id: u16,
) -> Result<Option<Vec<u8>>> {
    if !is_csdb_category(category_id) {
        return Ok(None);
    }

    let xml_url = format!(
        "{}?type=release&depth=1&id={}",
        WEBSERVICE_URL,
        urlencoding::encode(item_id)
    );

    let resp = http.get(&xml_url).send().await?;
    if !resp.status().is_success() {
        // CSDB returns 200 even for unknown ids (with empty XML), so a
        // non-2xx here is a real network problem worth surfacing.
        return Err(anyhow::anyhow!("CSDB webservice HTTP {}", resp.status()));
    }
    let xml = resp.text().await?;

    let url = match extract_screenshot_url(&xml) {
        Some(u) => u,
        None => return Ok(None),
    };

    let img_resp = http.get(&url).send().await?;
    if !img_resp.status().is_success() {
        return Err(anyhow::anyhow!(
            "screenshot HTTP {} for {}",
            img_resp.status(),
            url
        ));
    }
    Ok(Some(img_resp.bytes().await?.to_vec()))
}

/// Extract the contents of the first `<ScreenShot>...</ScreenShot>` element.
/// Plain substring scan — CSDB's XML is simple and the tag is unique within
/// a release record.
fn extract_screenshot_url(xml: &str) -> Option<String> {
    let open = "<ScreenShot>";
    let close = "</ScreenShot>";
    let start = xml.find(open)? + open.len();
    let end = xml[start..].find(close)? + start;
    let raw = xml[start..end].trim();
    if raw.is_empty() {
        None
    } else {
        Some(raw.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extract_url_from_typical_xml() {
        let xml = r#"<CSDbData><Release><ID>12345</ID><ScreenShot>https://csdb.dk/gfx/releases/12000/12345.png</ScreenShot></Release></CSDbData>"#;
        assert_eq!(
            extract_screenshot_url(xml).as_deref(),
            Some("https://csdb.dk/gfx/releases/12000/12345.png")
        );
    }

    #[test]
    fn extract_url_trims_whitespace() {
        let xml = "<ScreenShot>\n  https://example.com/x.png  \n</ScreenShot>";
        assert_eq!(
            extract_screenshot_url(xml).as_deref(),
            Some("https://example.com/x.png")
        );
    }

    #[test]
    fn missing_tag_returns_none() {
        assert!(extract_screenshot_url("<CSDbData></CSDbData>").is_none());
    }

    #[test]
    fn empty_tag_returns_none() {
        assert!(extract_screenshot_url("<ScreenShot></ScreenShot>").is_none());
    }
}
