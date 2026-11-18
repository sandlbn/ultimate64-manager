//! Capability gating for calls that only exist on newer Ultimate builds.
//!
//! A batch of REST calls (native disk-image creation, the menu screen, heap
//! statistics, file info, keyboard/joystick injection) is absent from older
//! firmware, which answers them with a 404. Every such call in this app is
//! guarded by [`DeviceCaps`].
//!
//! # Why the gate detects instead of comparing versions
//!
//! A version number identifies a firmware *line*, not a feature set. Gideon's
//! line (Ultimate II / II+ / II+L, Ultimate 64) is at 3.x and gained these
//! calls in 3.15. The Commodore-branded C64 Ultimate numbers itself
//! independently: it reported `1.1.0` when it lacked the calls and reports
//! `1.2.1` now that it has them. A `>= 3.15` compare therefore switched the
//! whole feature set off on a device that answers every one of these calls.
//!
//! So the version is only a fast path. `>= 3.15` is trusted as "has the API"
//! without a round-trip; anything else is settled by asking the device once
//! (see [`crate::api_315::probe_api`]) and remembering the answer for the
//! connection. Until that answer is in, the gate stays closed, so a device
//! that has not been asked keeps the paths that already worked.
//!
//! # What the gate still cannot express
//!
//! A call can exist in the firmware and be unavailable on the hardware in
//! front of you. `machine:input` answers **501** on a cartridge, because
//! driving keyboard and joystick lines needs Ultimate 64-class hardware. That
//! is a per-call answer, not a property of the firmware, so callers must treat
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
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DeviceCaps {
    firmware: Option<FirmwareVersion>,
    /// What the device itself said when asked whether it has the API.
    /// `None` until it has been asked — which is not the same as "no".
    probed: Option<bool>,
}

impl DeviceCaps {
    /// The capabilities a reported firmware version implies on its own.
    ///
    /// Only `>= 3.15` is conclusive. Everything else — an older 3.x, the
    /// Commodore line, an unparseable or absent version — is left closed but
    /// undecided: see [`Self::needs_probe`].
    pub fn from_firmware(raw: Option<&str>) -> Self {
        Self {
            firmware: raw.and_then(FirmwareVersion::parse),
            probed: None,
        }
    }

    /// Whether the version number alone proves the API is there.
    fn version_implies_api(&self) -> bool {
        self.firmware.as_ref().is_some_and(|v| v.at_least(FW_315))
    }

    /// Whether the device still has to be asked.
    ///
    /// False once the version settles it or an answer has been recorded, so
    /// the probe costs one request per connection rather than one per poll.
    pub fn needs_probe(&self) -> bool {
        !self.version_implies_api() && self.probed.is_none()
    }

    /// Record the device's own answer to the capability probe.
    pub fn with_probe_result(mut self, has_api: bool) -> Self {
        self.probed = Some(has_api);
        self
    }

    /// Carry over an answer already obtained on this connection.
    ///
    /// Only when the firmware string is unchanged: a firmware update happens
    /// without the app reconnecting, and the answer belongs to the build that
    /// gave it.
    pub fn inheriting_probe_from(mut self, earlier: &DeviceCaps) -> Self {
        if self.firmware == earlier.firmware {
            self.probed = earlier.probed;
        }
        self
    }

    /// Whether the REST additions are present — proven by the version on the
    /// 3.x line, or by the device's own answer everywhere else.
    ///
    /// The name records where the calls first appeared (firmware 3.15), not a
    /// requirement that the device report that number.
    pub fn has_315_api(&self) -> bool {
        self.version_implies_api() || self.probed == Some(true)
    }

    /// Guard helper: `Err(CapError::TooOld)` when the device lacks the calls.
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
    /// The firmware does not have the call.
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
                "not available on this firmware (device reports {}; the Ultimate 3.x line gained it in 3.15)",
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

    /// The Commodore line numbers itself independently of the 3.x line, so
    /// its versions land below the cut whether or not the build has the calls
    /// (1.1.0 did not, 1.2.1 does). The compare must not be read as an answer
    /// there — it only says the fast path does not apply.
    #[test]
    fn the_commodore_version_line_is_undecided_by_the_number_alone() {
        for fw in ["1.1.0", "1.2.1"] {
            assert!(!v(fw).at_least(FW_315));
            let caps = DeviceCaps::from_firmware(Some(fw));
            assert!(caps.needs_probe(), "{fw} must be asked, not assumed old");
            assert!(!caps.has_315_api(), "{fw} stays closed until it answers");
        }
    }

    /// The regression this module exists to prevent: a C64 Ultimate on 1.2.1
    /// answers every gated call, and a version compare switched them all off.
    #[test]
    fn a_device_that_answers_the_probe_has_the_api_whatever_it_calls_itself() {
        let caps = DeviceCaps::from_firmware(Some("1.2.1")).with_probe_result(true);
        assert!(caps.has_315_api());
        assert!(!caps.needs_probe(), "one answer per connection is enough");
        assert!(caps.require_315().is_ok());
    }

    #[test]
    fn a_device_that_refuses_the_probe_stays_gated_and_is_not_asked_again() {
        let caps = DeviceCaps::from_firmware(Some("1.1.0")).with_probe_result(false);
        assert!(!caps.has_315_api());
        assert!(!caps.needs_probe());
    }

    /// The 3.x fast path: no round-trip, and nothing a probe says can take the
    /// API away from a version that is known to have it.
    #[test]
    fn firmware_315_is_trusted_without_a_probe() {
        let caps = DeviceCaps::from_firmware(Some("3.15"));
        assert!(caps.has_315_api());
        assert!(!caps.needs_probe());
        assert!(caps.with_probe_result(false).has_315_api());
    }

    /// A device whose version cannot be read used to be written off as old.
    /// It is now asked like any other, and stays closed until it answers.
    #[test]
    fn an_unreadable_version_is_asked_rather_than_assumed_old() {
        for raw in [None, Some("unknown")] {
            let caps = DeviceCaps::from_firmware(raw);
            assert!(caps.needs_probe());
            assert!(!caps.has_315_api());
            assert!(caps.with_probe_result(true).has_315_api());
        }
    }

    /// The answer is cached across status polls, but it belongs to the build
    /// that gave it: after a firmware update the device must be asked again.
    #[test]
    fn a_cached_answer_survives_a_poll_but_not_a_firmware_change() {
        let earlier = DeviceCaps::from_firmware(Some("1.2.1")).with_probe_result(true);

        let same = DeviceCaps::from_firmware(Some("1.2.1")).inheriting_probe_from(&earlier);
        assert!(same.has_315_api());
        assert!(!same.needs_probe());

        let updated = DeviceCaps::from_firmware(Some("1.3.0")).inheriting_probe_from(&earlier);
        assert!(updated.needs_probe());
        assert!(!updated.has_315_api());
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
    fn unparseable_or_absent_version_is_closed_until_probed() {
        assert!(FirmwareVersion::parse("").is_none());
        assert!(FirmwareVersion::parse("unknown").is_none());
        assert!(!DeviceCaps::from_firmware(Some("unknown")).has_315_api());
        assert!(!DeviceCaps::from_firmware(None).has_315_api());
    }

    #[test]
    fn caps_gate_matches_the_live_devices() {
        // Ultimate II+L on 3.15 — has the calls; the version proves it.
        assert!(DeviceCaps::from_firmware(Some("3.15")).has_315_api());
        // C64 Ultimate on 1.1.0 — answered 404 to all of them.
        assert!(!DeviceCaps::from_firmware(Some("1.1.0"))
            .with_probe_result(false)
            .has_315_api());
        // The same product on 1.2.1 — answers them.
        assert!(DeviceCaps::from_firmware(Some("1.2.1"))
            .with_probe_result(true)
            .has_315_api());
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
