//! Which of the device's network interfaces we are actually talking to.
//!
//! The VIC video stream only works over the wired interface. On Wi-Fi the
//! device still accepts `streams/video:start` and answers `200 OK`, then sends
//! essentially nothing — measured on a C64 Ultimate: 3405 packets in a second
//! over Ethernet versus a single 2-byte packet over Wi-Fi. Nothing in the
//! response distinguishes the two, so without this the app waits for frames
//! that will never arrive and the user is given no reason why.
//!
//! Firmware that reports `ethernet_mac` and `wifi_mac` makes the question
//! answerable: resolve the MAC behind the address we are connected to and see
//! which one it is. The device commonly has an address on *both* interfaces at
//! once, so "is this device wired?" is the wrong question — it is this
//! *connection* that is or isn't.

/// Which interface a connection is running over.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LinkKind {
    Wired,
    WiFi,
    /// The address could not be matched to either interface — an older
    /// firmware that reports no MACs, an address reached through a router (so
    /// the local ARP entry is the gateway's), or simply no ARP entry yet.
    Unknown,
}

impl LinkKind {
    /// Whether video streaming is known not to work on this link.
    pub fn blocks_video_stream(self) -> bool {
        self == LinkKind::WiFi
    }
}

/// Parse a MAC into octets, tolerating the forms these two sources disagree on:
/// the firmware reports `02:15:41:43:29:FC` (padded, upper case) while the
/// system ARP table prints `2:15:41:43:29:fc` (unpadded, lower case).
pub fn parse_mac(s: &str) -> Option<[u8; 6]> {
    let parts: Vec<&str> = s.trim().split(':').collect();
    if parts.len() != 6 {
        return None;
    }
    let mut out = [0u8; 6];
    for (i, p) in parts.iter().enumerate() {
        if p.is_empty() || p.len() > 2 {
            return None;
        }
        out[i] = u8::from_str_radix(p, 16).ok()?;
    }
    Some(out)
}

/// Decide the link from the MAC behind `ip` and the MACs the device reports.
///
/// Kept separate from the lookup so the decision is testable without touching
/// the machine's ARP table.
pub fn classify(peer_mac: Option<[u8; 6]>, ethernet: Option<&str>, wifi: Option<&str>) -> LinkKind {
    let Some(peer) = peer_mac else {
        return LinkKind::Unknown;
    };
    if ethernet.and_then(parse_mac) == Some(peer) {
        return LinkKind::Wired;
    }
    if wifi.and_then(parse_mac) == Some(peer) {
        return LinkKind::WiFi;
    }
    LinkKind::Unknown
}

/// Look up the MAC for `ip` in the system ARP table.
///
/// Only meaningful for an address on the local segment: anything routed
/// resolves to the gateway, which matches neither MAC and so reads as
/// `Unknown` — the honest answer rather than a wrong guess.
pub fn arp_lookup(ip: &str) -> Option<[u8; 6]> {
    let out = std::process::Command::new("arp")
        .args(["-n", ip])
        .output()
        .ok()?;
    parse_arp_output(&String::from_utf8_lossy(&out.stdout))
}

/// Pull the first MAC out of an `arp -n` line.
fn parse_arp_output(text: &str) -> Option<[u8; 6]> {
    for token in text.split_whitespace() {
        if token.matches(':').count() == 5 {
            if let Some(mac) = parse_mac(token) {
                return Some(mac);
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The two sources format MACs differently; both must parse to the same
    /// octets or every comparison silently fails.
    #[test]
    fn padded_and_unpadded_forms_compare_equal() {
        let firmware = parse_mac("02:15:41:43:29:FC").expect("firmware form");
        let arp = parse_mac("2:15:41:43:29:fc").expect("arp form");
        assert_eq!(firmware, arp);
    }

    #[test]
    fn malformed_macs_are_rejected_rather_than_guessed() {
        assert!(parse_mac("").is_none());
        assert!(parse_mac("02:15:41:43:29").is_none(), "too few octets");
        assert!(parse_mac("02:15:41:43:29:FC:AB").is_none(), "too many");
        assert!(parse_mac("zz:15:41:43:29:fc").is_none(), "not hex");
        assert!(parse_mac("021:15:41:43:29:fc").is_none(), "octet too long");
    }

    /// The real values from the device this was built against.
    #[test]
    fn the_live_addresses_classify_to_their_interfaces() {
        const ETH: &str = "02:15:41:43:29:FC";
        const WIFI: &str = "9C:13:9E:EF:BF:50";

        // 10.0.0.46 answered with the ethernet MAC.
        let wired = classify(parse_mac("2:15:41:43:29:fc"), Some(ETH), Some(WIFI));
        assert_eq!(wired, LinkKind::Wired);
        assert!(!wired.blocks_video_stream());

        // 10.0.0.139 answered with the wifi MAC.
        let wifi = classify(parse_mac("9c:13:9e:ef:bf:50"), Some(ETH), Some(WIFI));
        assert_eq!(wifi, LinkKind::WiFi);
        assert!(wifi.blocks_video_stream(), "video does not work over wifi");
    }

    /// Firmware too old to report MACs must not be guessed at: claiming "wired"
    /// would hide a real problem, claiming "wifi" would nag without cause.
    #[test]
    fn a_device_that_reports_no_macs_is_unknown() {
        let k = classify(parse_mac("2:15:41:43:29:fc"), None, None);
        assert_eq!(k, LinkKind::Unknown);
        assert!(!k.blocks_video_stream(), "never warn on a guess");
    }

    /// A routed address resolves to the gateway, matching neither interface.
    #[test]
    fn an_unrelated_mac_is_unknown_not_wifi() {
        let k = classify(
            parse_mac("aa:bb:cc:dd:ee:ff"),
            Some("02:15:41:43:29:FC"),
            Some("9C:13:9E:EF:BF:50"),
        );
        assert_eq!(k, LinkKind::Unknown);
    }

    #[test]
    fn no_arp_entry_is_unknown() {
        assert_eq!(
            classify(None, Some("02:15:41:43:29:FC"), Some("9C:13:9E:EF:BF:50")),
            LinkKind::Unknown
        );
    }

    #[test]
    fn arp_output_is_parsed_from_a_real_line() {
        // macOS form
        let mac = parse_arp_output("? (10.0.0.139) at 9c:13:9e:ef:bf:50 on en0 ifscope [ethernet]");
        assert_eq!(mac, parse_mac("9c:13:9e:ef:bf:50"));
        // Linux form
        let mac = parse_arp_output("10.0.0.46  ether  02:15:41:43:29:fc  C  eth0");
        assert_eq!(mac, parse_mac("02:15:41:43:29:fc"));
        // An entry with no MAC yet
        assert_eq!(
            parse_arp_output("? (10.0.0.9) at (incomplete) on en0"),
            None
        );
    }
}
