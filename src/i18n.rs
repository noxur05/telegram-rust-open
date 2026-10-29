use std::collections::HashMap;
use std::sync::LazyLock;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum Lang {
    /// Turkmen is both the default and the fallback locale for missing keys.
    #[default]
    Tk,
    Ru,
    En,
}

impl Lang {
    pub fn from_code(s: &str) -> Self {
        match s {
            "ru" => Lang::Ru,
            "en" => Lang::En,
            _ => Lang::Tk,
        }
    }

    pub fn as_code(&self) -> &'static str {
        match self {
            Lang::Tk => "tk",
            Lang::Ru => "ru",
            Lang::En => "en",
        }
    }
}

type LocaleMap = HashMap<String, String>;

static LOCALES: LazyLock<HashMap<Lang, LocaleMap>> = LazyLock::new(|| {
    let mut m = HashMap::new();

    let tk: LocaleMap =
        serde_json::from_str(include_str!("../locales/tk.json")).expect("Failed to parse tk.json");
    let ru: LocaleMap =
        serde_json::from_str(include_str!("../locales/ru.json")).expect("Failed to parse ru.json");
    let en: LocaleMap =
        serde_json::from_str(include_str!("../locales/en.json")).expect("Failed to parse en.json");

    m.insert(Lang::Tk, tk);
    m.insert(Lang::Ru, ru);
    m.insert(Lang::En, en);
    m
});

/// Get a translated string by key and language.
/// Falls back to Turkmen if key not found in the requested language.
/// Panics if key is missing from all languages.
pub fn t(key: &str, lang: Lang) -> &'static str {
    let locales = &*LOCALES;
    if let Some(map) = locales.get(&lang) {
        if let Some(val) = map.get(key) {
            return val.as_str();
        }
    }
    // Fallback to Turkmen
    locales[&Lang::Tk]
        .get(key)
        .map(|s| s.as_str())
        .unwrap_or_else(|| panic!("Missing i18n key: {}", key))
}

/// Get a translated string with template variable substitution.
/// Variables use `{name}` syntax. Values should be pre-escaped for MarkdownV2 if needed.
pub fn tf(key: &str, lang: Lang, vars: &[(&str, &str)]) -> String {
    let template = t(key, lang);
    let mut result = template.to_string();
    for (name, value) in vars {
        result = result.replace(&format!("{{{}}}", name), value);
    }
    result
}

/// Represents a button action that can be matched from any language's button text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ButtonAction {
    Balance,
    Buy,
    Subscriptions,
    Help,
    Support,
    Lang,
}

/// Button key to action mapping.
static BUTTON_KEYS: &[(&str, ButtonAction)] = &[
    ("btn_balance", ButtonAction::Balance),
    ("btn_buy", ButtonAction::Buy),
    ("btn_subscriptions", ButtonAction::Subscriptions),
    ("btn_help", ButtonAction::Help),
    ("btn_support", ButtonAction::Support),
    ("btn_lang", ButtonAction::Lang),
];

/// Pre-built reverse lookup: button text -> ButtonAction
static BUTTON_LOOKUP: LazyLock<HashMap<&'static str, ButtonAction>> = LazyLock::new(|| {
    let mut map = HashMap::new();
    for lang in [Lang::Tk, Lang::Ru, Lang::En] {
        for (key, action) in BUTTON_KEYS {
            let text = t(key, lang);
            map.insert(text, *action);
        }
    }
    map
});

/// Match a button press text against all languages' button labels.
/// Returns the corresponding ButtonAction if found.
pub fn match_button(text: &str) -> Option<ButtonAction> {
    BUTTON_LOOKUP.get(text).copied()
}

#[cfg(test)]
mod tests {
    use super::*;

    const LANGS: [Lang; 3] = [Lang::Tk, Lang::Ru, Lang::En];

    /// Every locale must define exactly the same keys.
    ///
    /// `t()` silently falls back to Turkmen for a key missing from ru/en, so a
    /// half-translated key ships as the wrong language instead of failing loudly.
    #[test]
    fn all_locales_define_the_same_keys() {
        let tk: std::collections::BTreeSet<_> = LOCALES[&Lang::Tk].keys().collect();
        for lang in [Lang::Ru, Lang::En] {
            let other: std::collections::BTreeSet<_> = LOCALES[&lang].keys().collect();
            let missing: Vec<_> = tk.difference(&other).collect();
            let extra: Vec<_> = other.difference(&tk).collect();
            assert!(
                missing.is_empty() && extra.is_empty(),
                "{:?}: missing {:?}, extra {:?}",
                lang,
                missing,
                extra
            );
        }
    }

    /// Templates that carry backslash escapes are MarkdownV2, and Telegram rejects
    /// the whole message with 400 if any special character is left unescaped. Every
    /// send site uses `let _ = ...`, so such a failure is completely silent — the
    /// user taps a button and nothing happens. That exact bug shipped once already
    /// (Help -> Pricing with an empty plan table), so it gets a regression test.
    ///
    /// `*`, `_` and `` ` `` are the markup we write deliberately; inside a code span
    /// only a backtick is special, which is why code spans are tracked.
    #[test]
    fn markdown_v2_templates_are_fully_escaped() {
        const MUST_ESCAPE: &str = "[]()~>#+-=|{}.!";

        for lang in LANGS {
            for (key, template) in LOCALES[&lang].iter() {
                if !template.contains('\\') {
                    continue; // plain-text string, sent without a parse mode
                }

                let chars: Vec<char> = template.chars().collect();
                let (mut i, mut in_code, mut stars) = (0usize, false, 0usize);
                while i < chars.len() {
                    match chars[i] {
                        '\\' => {
                            i += 2;
                            continue;
                        }
                        '`' => in_code = !in_code,
                        '*' if !in_code => stars += 1,
                        // {placeholder} values are escaped by the caller, not here
                        '{' if !in_code => {
                            if let Some(off) = chars[i..].iter().position(|&c| c == '}') {
                                i += off + 1;
                                continue;
                            }
                        }
                        c if !in_code && MUST_ESCAPE.contains(c) => panic!(
                            "{:?}/{}: unescaped MarkdownV2 char {:?} — Telegram will \
                             reject this message with 400 and the send is silently ignored",
                            lang, key, c
                        ),
                        _ => {}
                    }
                    i += 1;
                }

                assert!(!in_code, "{:?}/{}: unbalanced backticks", lang, key);
                assert_eq!(stars % 2, 0, "{:?}/{}: unbalanced '*' (bold)", lang, key);
            }
        }
    }

    /// Button labels are sent as inline-keyboard text, which is never parsed as
    /// MarkdownV2 — an escape there renders as a literal backslash to the user.
    #[test]
    fn button_labels_carry_no_markdown_escapes() {
        for lang in LANGS {
            for (key, value) in LOCALES[&lang].iter() {
                if key.starts_with("btn_") {
                    assert!(
                        !value.contains('\\'),
                        "{:?}/{}: button label contains an escape: {:?}",
                        lang,
                        key,
                        value
                    );
                }
            }
        }
    }

    /// Every `support_*` string is sent by `handlers/support.rs` with NO parse mode —
    /// grep the file: not one `send_message` there sets `.parse_mode(...)`. So a
    /// MarkdownV2 escape in one of them is not "safe extra escaping", it renders to the
    /// user as a literal backslash: `iberiň\.` instead of `iberiň.`
    ///
    /// This is the mirror of `button_labels_carry_no_markdown_escapes`, and it exists
    /// because two new support strings shipped with `\.` and `\-` in them.
    #[test]
    fn support_strings_are_plain_text_and_carry_no_escapes() {
        for lang in LANGS {
            for (key, value) in LOCALES[&lang].iter() {
                if key.starts_with("support_") {
                    assert!(
                        !value.contains('\\'),
                        "{:?}/{}: sent without a parse mode, so an escape renders \
                         literally to the user: {:?}",
                        lang,
                        key,
                        value
                    );
                }
            }
        }
    }

    /// Keyboard button text is matched back to an action by exact string compare,
    /// so two languages sharing a label would make one of them unreachable.
    #[test]
    fn keyboard_button_labels_are_unique_across_languages() {
        let mut seen = std::collections::HashMap::new();
        for lang in LANGS {
            for (key, _) in BUTTON_KEYS {
                let label = t(key, lang);
                if let Some(prev) = seen.insert(label, (lang, *key)) {
                    assert_eq!(
                        prev.1, *key,
                        "label {:?} is shared by {} and {} — match_button would \
                         route one of them to the wrong action",
                        label, prev.1, key
                    );
                }
            }
        }
    }
}
