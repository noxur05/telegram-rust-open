//! The Marzban `note` (the panel calls it "remark") as this bot's record of an account.
//!
//! Identity used to live in the Marzban username: every account the bot created was
//! named `tg_{telegram_id}_{unix_ts}`, and `handlers::subscriptions::owns` decided
//! ownership by matching that prefix. That only worked while the bot chose the name.
//! Once a user may pick their own username the field becomes attacker-controlled — the
//! panel's own regex admits `tg_1317186982_9999999999` — and `subscription_url` is a
//! bearer credential, so a username-based ownership check would hand one user another
//! user's subscription.
//!
//! So the note carries identity instead. It is written only by this bot and is never
//! echoed back from user input, which is what makes it trustworthy where the username
//! is not.
//!
//! Format, one line, pipe-separated, id always first:
//!
//! ```text
//! TG:1317186982 | Basic $2.50 | 2026-08-30 | @bagtyyar
//! ```
//!
//! Only the id is load-bearing. Everything after it is for a human reading the panel,
//! and any of it may be absent on an older account.

/// Separator between fields. Stripped from every interpolated value, so a value can
/// never introduce a field.
const SEP: &str = " | ";

/// The panel rejects a note longer than this (`validate_note`, 500 characters).
pub const NOTE_MAX: usize = 500;

/// Marker introducing the telegram id. Chosen to match what the 32 pre-existing
/// bot-created accounts already carry (`TG:{id} Balance`), so those parse before they
/// are even rewritten.
const TG: &str = "TG:";

/// What we know about an account, as recorded in its note.
#[derive(Debug, PartialEq)]
pub struct Note {
    /// The owning telegram user. The only field ownership depends on.
    ///
    /// Deliberately NOT an `Option`. A note without an id is owned by nobody: it drops
    /// out of the listing, extend rejects it, and nothing can restore it, because no
    /// other record maps an account to a user. Making it non-optional turns
    /// "never write a note without its id" from a rule someone must remember into
    /// something the type system will not let them express.
    pub telegram_id: i64,
    /// Plan name and the price actually paid, e.g. `Basic $2.50`.
    pub plan: Option<String>,
    /// Purchase (or last extension) date, `YYYY-MM-DD`.
    pub date: Option<String>,
    /// The user's Telegram handle at the time, without the `@`.
    pub handle: Option<String>,
}

/// Strip anything from a value that could forge structure in the note.
///
/// Pipes would introduce a field; newlines would end the line; a literal `TG:` early in
/// a value could be mistaken for the id by a laxer parser than ours. Cheap insurance:
/// none of these belong in a plan name or a Telegram handle anyway.
fn clean(value: &str) -> String {
    value
        .replace('|', "/")
        .replace(['\n', '\r'], " ")
        .replace(TG, "TG-")
        .trim()
        .to_string()
}

impl Note {
    /// Render to the string stored on the panel, truncated to the panel's own cap.
    ///
    /// The id is written first and is never truncated away: a note that lost its id
    /// would orphan the account, since ownership has no other source.
    pub fn render(&self) -> String {
        let mut parts: Vec<String> = vec![format!("{}{}", TG, self.telegram_id)];
        for v in [&self.plan, &self.date, &self.handle].into_iter().flatten() {
            let c = clean(v);
            if !c.is_empty() {
                parts.push(c);
            }
        }
        let mut out = parts.join(SEP);
        if out.chars().count() > NOTE_MAX {
            out = out.chars().take(NOTE_MAX).collect();
        }
        out
    }
}

/// Read the owning telegram id out of a note.
///
/// **This is the ownership primitive.** Two properties are essential:
///
/// 1. The id must be delimited, not merely a prefix. Marzban's `?search=` is a
///    wildcarded `ILIKE` over username AND note, so a search for `TG:123` also returns
///    `TG:1234`. A prefix test would inherit exactly the bug `owns()` was written to
///    avoid, one field over.
/// 2. The marker must appear at the very start. Everything after the first field is
///    descriptive and, on a hand-edited note, could contain anything.
pub fn parse_telegram_id(note: &str) -> Option<i64> {
    let rest = note.trim_start().strip_prefix(TG)?;
    let digits: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
    if digits.is_empty() {
        return None;
    }
    // The character after the digits must end the id, never extend it.
    match rest[digits.len()..].chars().next() {
        None => {}
        Some(c) if c.is_whitespace() || c == '|' => {}
        Some(_) => return None,
    }
    digits.parse().ok()
}

/// Ashgabat is UTC+5 year round, with no daylight saving — the same offset every
/// timestamp in this codebase is already written with (`TIMEZONE('Asia/Ashgabat', …)`).
const ASHGABAT_OFFSET_SECS: i64 = 5 * 3600;

/// Format a unix timestamp as `YYYY-MM-DD` in Asia/Ashgabat.
///
/// Hand-rolled rather than pulling in `chrono` for one line of output: this crate has
/// eight dependencies and none of them is a date library. The conversion is Howard
/// Hinnant's `civil_from_days`, which is exact for every date this will ever see.
pub fn ymd(unix_secs: i64) -> String {
    let days = (unix_secs + ASHGABAT_OFFSET_SECS).div_euclid(86_400);
    // Shift the epoch to 0000-03-01 so leap days land at the end of the cycle.
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    format!("{:04}-{:02}-{:02}", y, m, d)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renders_the_documented_format() {
        let n = Note {
            telegram_id: 1317186982,
            plan: Some("Basic $2.50".into()),
            date: Some("2026-08-30".into()),
            handle: Some("@bagtyyar".into()),
        };
        assert_eq!(
            n.render(),
            "TG:1317186982 | Basic $2.50 | 2026-08-30 | @bagtyyar"
        );
        // Absent fields are omitted, never left as empty slots.
        let bare = Note {
            telegram_id: 7,
            plan: None,
            date: None,
            handle: None,
        };
        assert_eq!(bare.render(), "TG:7");
    }

    /// The 32 accounts already on the panel carry `TG:{id} Balance`. Ownership has to
    /// work on them from the moment this lands, before any of them are rewritten.
    #[test]
    fn reads_the_legacy_note_format() {
        assert_eq!(parse_telegram_id("TG:1317186982 Balance"), Some(1317186982));
    }

    /// The bug `owns()` exists to prevent, one field over. Marzban's `?search=` is a
    /// wildcarded ILIKE over username AND note, so `TG:123` also matches `TG:1234`.
    /// A prefix test here would leak one user's subscription_url to another.
    #[test]
    fn an_id_must_be_delimited_not_merely_a_prefix() {
        assert_eq!(parse_telegram_id("TG:123 | x"), Some(123));
        assert_eq!(parse_telegram_id("TG:123"), Some(123));
        assert_eq!(parse_telegram_id("TG:123 Balance"), Some(123));
        // 1234 must never read as 123.
        assert_eq!(parse_telegram_id("TG:1234 | x"), Some(1234));
        assert_ne!(parse_telegram_id("TG:1234 | x"), Some(123));
        // Anything that would extend the id is a refusal, not a truncation.
        assert_eq!(parse_telegram_id("TG:123abc"), None);
        assert_eq!(parse_telegram_id("TG:123-456"), None);
    }

    /// The marker must open the note. A note whose id appears later is untrusted text.
    #[test]
    fn the_marker_must_start_the_note() {
        assert_eq!(parse_telegram_id("owned by TG:999999999"), None);
        assert_eq!(parse_telegram_id("Yhhy | TG:999999999"), None);
        assert_eq!(parse_telegram_id(""), None);
        assert_eq!(parse_telegram_id("TG:"), None);
        assert_eq!(parse_telegram_id("Balance"), None);
    }

    /// Reseller accounts on this panel carry free text or nothing. None may parse as
    /// owned by anyone.
    #[test]
    fn real_reseller_notes_are_never_owned() {
        for n in [
            "",
            "Yhhy",
            "yhhy",
            "yhhyservers",
            "Men şirnazar",
            "Development",
            "+ Aynur gelin",
        ] {
            assert_eq!(parse_telegram_id(n), None, "{n:?} must not parse as owned");
        }
    }

    /// A value must never be able to introduce a field or forge an id.
    #[test]
    fn interpolated_values_cannot_forge_structure() {
        let evil = Note {
            telegram_id: 111,
            plan: Some("x | TG:999999999".into()),
            date: Some("2026-01-01\nTG:888".into()),
            handle: Some("@a|b".into()),
        };
        let s = evil.render();
        assert_eq!(parse_telegram_id(&s), Some(111), "id hijacked: {s}");
        assert!(!s.contains('\n'));
        // The forged markers are defanged, and the real id is still the only one at the front.
        assert!(s.starts_with("TG:111 "));
        assert_eq!(
            s.matches("TG:").count(),
            1,
            "a value smuggled in a marker: {s}"
        );
    }

    /// The panel rejects a note over 500 characters outright, which would fail the
    /// write and leave the account unrecorded.
    #[test]
    fn never_exceeds_the_panels_note_limit() {
        let n = Note {
            telegram_id: 1317186982,
            plan: Some("P".repeat(400)),
            date: Some("D".repeat(400)),
            handle: Some("H".repeat(400)),
        };
        let s = n.render();
        assert!(s.chars().count() <= NOTE_MAX, "{} chars", s.chars().count());
        // Truncation must never cost us the id.
        assert_eq!(parse_telegram_id(&s), Some(1317186982));
    }

    /// Vectors generated from Python's calendar, not written by hand — two of my
    /// hand-written expectations were wrong (2026 is not a leap year). The algorithm
    /// was also fuzzed against that reference over 200k timestamps spanning 1970..2103
    /// with zero mismatches, including the 1900/2000/2100 century-leap rules.
    #[test]
    fn ymd_converts_known_timestamps() {
        assert_eq!(ymd(0), "1970-01-01");
        assert_eq!(ymd(1_767_225_600), "2026-01-01");
        assert_eq!(ymd(1_709_164_800), "2024-02-29"); // 2024 IS a leap year
        assert_eq!(ymd(4_102_444_800), "2100-01-01"); // /100 -> NOT a leap year
        assert_eq!(ymd(946_684_800), "2000-01-01"); // /400 -> IS a leap year
                                                    // The +5 offset must shift the DAY, not merely the hour.
        assert_eq!(ymd(1_767_222_000), "2026-01-01"); // 23:00Z on the 31st
        assert_eq!(ymd(1_767_204_000), "2025-12-31"); // 18:00Z on the 31st
    }
}
