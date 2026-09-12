//! Firmware-version gating for calls that only exist on newer Ultimate builds.
//!
//! Ultimate firmware 3.15 added a batch of REST calls (native disk-image
//! creation, the menu screen, heap statistics, file info, keyboard/joystick
//! injection). Calling them on older firmware gets a 404, so every new call in
//! this app is guarded by [`DeviceCaps`].
//!
//! # Why a version compare is enough, and where it isn't
//!
//! The 3.x line is Gideon's board (Ultimate II / II+ / II+L and the Ultimate 64
//! II), so a numeric `>= 3.15` cleanly separates old from new across that whole
//! family. The Commodore-branded C64 Ultimate fork numbers itself separately
//! (it reports `1.1.0`) and genuinely lacks these calls — verified: it answers
//! 404 on every one of them — so falling below the cut is the correct outcome
//! there rather than an accident of string comparison.
//!
//! What a version compare *cannot* express is that a call can exist in the
//! firmware and still be unavailable on the hardware in front of you.
//! `machine:input` is registered on 3.15 but answers **501** on a cartridge,
//! because driving keyboard and joystick lines needs Ultimate 64-class
//! hardware that a cartridge does not have. So callers must treat
//! [`CapError::Unsupported`] as a normal outcome and fall back, not as a bug.

/// A firmware version, parsed into comparable numeric components.
///
/// Kept as a `Vec` rather than a fixed triple because the two product lines
/// don't agree on how many components they publish — `3.15` has two, `1.1.0`
/// has three — and comparing them still has to work.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FirmwareVersion {
    parts: Vec<u32>,
    raw: String,
}

impl FirmwareVersion {
    /// Parse a version as reported by `/v1/info`.
    ///
    /// Tolerates trailing junk on a component (`"3.15-rc1"` → `[3, 15]`) so a
    /// pre-release build isn't misread as older than the release it precedes.
    pub fn parse(raw: &str) -> Option<Self> {
        let parts: Vec<u32> = raw
            .trim()
            .split('.')
            .map(|p| {
                let digits: String = p.chars().take_while(|c| c.is_ascii_digit()).collect();
                digits.parse::<u32>().ok()
            })
            .collect::<Option<Vec<u32>>>()?;
        if parts.is_empty() {
            return None;
        }
        Some(Self {
            parts,
            raw: raw.trim().to_string(),
        })
    }

    /// The version string exactly as the device reported it.
    pub fn raw(&self) -> &str {
        &self.raw
    }

    /// Whether this version is at least `other`. Missing trailing components
    /// count as zero, so `3.15` and `3.15.0` compare equal.
    pub fn at_least(&self, other: &[u32]) -> bool {
        let len = self.parts.len().max(other.len());
        for i in 0..len {
            let a = self.parts.get(i).copied().unwrap_or(0);
            let b = other.get(i).copied().unwrap_or(0);
            match a.cmp(&b) {
                std::cmp::Ordering::Greater => return true,
                std::cmp::Ordering::Less => return false,
                std::cmp::Ordering::Equal => {}
            }
        }
        true
    }
}

/// The firmware release that introduced the calls guarded here.
pub const FW_315: &[u32] = &[3, 15];

/// What the connected device supports.
#[derive(Debug, Clone, Default)]
pub struct DeviceCaps {
    firmware: Option<FirmwareVersion>,
}

impl DeviceCaps {
    /// Read the capabilities implied by a reported firmware version. An
    /// unparseable or absent version is treated as old, so a device that
    /// reports something unexpected keeps the paths that already worked.
    pub fn from_firmware(raw: Option<&str>) -> Self {
        Self {
            firmware: raw.and_then(FirmwareVersion::parse),
        }
    }

    /// Whether the 3.15 REST additions are present.
    pub fn has_315_api(&self) -> bool {
        self.firmware.as_ref().is_some_and(|v| v.at_least(FW_315))
    }

    /// Guard helper: `Err(CapError::TooOld)` when the firmware predates 3.15.
    pub fn require_315(&self) -> Result<(), CapError> {
        if self.has_315_api() {
            Ok(())
        } else {
            Err(CapError::TooOld {
                found: self
                    .firmware
                    .as_ref()
                    .map(|v| v.raw().to_string())
                    .unwrap_or_else(|| "unknown".to_string()),
            })
        }
    }
}

/// Why a version-gated call could not run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CapError {
    /// The firmware is older than the release that added the call.
    TooOld { found: String },
    /// The firmware has the call, but this hardware cannot perform it (HTTP
    /// 501). A cartridge answering `machine:input` is the standard case.
    Unsupported(String),
    /// The device understood the call and refused it — a missing path, a name
    /// already taken, a bad argument. Carries the firmware's own wording.
    ///
    /// Kept distinct from the two above because they say "you cannot do this
    /// here" while this one says "that particular request was wrong", and
    /// conflating them produces nonsense like "needs firmware 3.15 or newer
    /// (device reports FILE EXISTS)".
    Device(String),
}

impl std::fmt::Display for CapError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CapError::TooOld { found } => write!(
                f,
                "needs Ultimate firmware 3.15 or newer (device reports {})",
                found
            ),
            CapError::Unsupported(why) => f.write_str(why),
            CapError::Device(why) => f.write_str(why),
        }
    }
}

impl From<CapError> for String {
    fn from(e: CapError) -> String {
        e.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(s: &str) -> FirmwareVersion {
        FirmwareVersion::parse(s).expect("should parse")
    }

    #[test]
    fn parses_two_and_three_component_versions() {
        assert_eq!(v("3.15").parts, vec![3, 15]);
        assert_eq!(v("1.1.0").parts, vec![1, 1, 0]);
        assert_eq!(v("3.15").raw(), "3.15");
    }

    /// 3.15 is the cut. 3.14 is the release this app was previously tested
    /// against, and 3.9 guards against a string compare sneaking back in —
    /// "3.9" > "3.15" alphabetically but is older.
    #[test]
    fn the_315_cut_is_numeric_not_alphabetic() {
        assert!(v("3.15").at_least(FW_315));
        assert!(v("3.15.1").at_least(FW_315));
        assert!(v("3.16").at_least(FW_315));
        assert!(v("4.0").at_least(FW_315));
        assert!(!v("3.14").at_least(FW_315));
        assert!(!v("3.9").at_least(FW_315), "3.9 is older than 3.15");
    }

    /// The Commodore-fork C64 Ultimate numbers itself on a separate line and
    /// does not have these calls (verified: 404 on all of them), so it must
    /// fall below the cut.
    #[test]
    fn the_commodore_fork_version_line_falls_below_the_cut() {
        assert!(!v("1.1.0").at_least(FW_315));
    }

    #[test]
    fn missing_trailing_components_count_as_zero() {
        assert!(v("3.15").at_least(&[3, 15, 0]));
        assert!(v("3.15.0").at_least(FW_315));
        assert!(!v("3").at_least(FW_315));
    }

    /// A pre-release must not read as older than the release it precedes.
    #[test]
    fn tolerates_suffixed_components() {
        assert!(v("3.15-rc1").at_least(FW_315));
    }

    /// Ultimate 64 hardware publishes a letter-suffixed patch level — the
    /// firmware's own docs give `Ultimate 64 Elite (V1.49) 3.14d` — so the
    /// suffix must neither break parsing nor round the version up.
    #[test]
    fn the_ultimate64_letter_suffixed_format_compares_correctly() {
        assert_eq!(v("3.14d").parts, vec![3, 14]);
        assert!(!v("3.14d").at_least(FW_315), "3.14d is still before 3.15");
        assert!(v("3.15d").at_least(FW_315));
    }

    /// 3.15a is a real released build. A letter-suffixed patch level must pass
    /// the gate: it is 3.15 with a revision letter, not something before it.
    #[test]
    fn the_released_letter_suffixed_builds_gate_correctly() {
        for (v, expected) in [
            ("3.15", true),
            ("3.15a", true), // released
            ("3.15d", true),
            ("3.16a", true),
            ("3.14", false),
            ("3.14d", false), // Ultimate 64 Elite's own published format
            ("3.9", false),
            ("3.9z", false),
            ("1.1.0", false),
        ] {
            let parsed = FirmwareVersion::parse(v).unwrap_or_else(|| panic!("{v} must parse"));
            assert_eq!(
                parsed.at_least(FW_315),
                expected,
                "{v} -> parts {:?}",
                parsed.parts
            );
            assert_eq!(
                DeviceCaps::from_firmware(Some(v)).has_315_api(),
                expected,
                "{v} through DeviceCaps"
            );
        }
    }

    #[test]
    fn unparseable_or_absent_version_is_treated_as_old() {
        assert!(FirmwareVersion::parse("").is_none());
        assert!(FirmwareVersion::parse("unknown").is_none());
        assert!(!DeviceCaps::from_firmware(Some("unknown")).has_315_api());
        assert!(!DeviceCaps::from_firmware(None).has_315_api());
    }

    #[test]
    fn caps_gate_matches_the_two_live_devices() {
        // Ultimate II+L on 3.15 — has the new calls.
        assert!(DeviceCaps::from_firmware(Some("3.15")).has_315_api());
        // C64 Ultimate on 1.1.0 — does not.
        assert!(!DeviceCaps::from_firmware(Some("1.1.0")).has_315_api());
    }

    #[test]
    fn require_315_names_the_version_it_found() {
        let err = DeviceCaps::from_firmware(Some("3.14"))
            .require_315()
            .expect_err("3.14 must be refused");
        let msg = err.to_string();
        assert!(msg.contains("3.15"), "{msg}");
        assert!(msg.contains("3.14"), "should name what it found: {msg}");
    }
}
