use std::collections::{HashMap, HashSet};
use std::sync::LazyLock;
use tokio::sync::Mutex;

use teloxide::prelude::*;
use teloxide::types::{ChatId, InputFile, Message, UserId};

use sea_orm::DatabaseConnection;

use crate::config::get_config;
use crate::db::get_user_language;
use crate::i18n::{t, tf, Lang};

/// Set of user IDs currently waiting to type a support message.
static WAITING_FOR_SUPPORT: LazyLock<Mutex<HashSet<i64>>> =
    LazyLock::new(|| Mutex::new(HashSet::new()));

/// The album a user is part-way through sending, keyed by user id.
///
/// Telegram delivers an album as one update PER photo, all sharing a `media_group_id`.
/// Support mode is one-shot and ends on the first part, so parts 2..n are admitted on
/// this key instead. An earlier attempt solved the same problem by simply NOT ending
/// support mode on an attachment — which left the user in support mode indefinitely,
/// since no inline callback clears the flag: they could tap "Top up → Custom", type an
/// amount, and every file they ever sent afterwards would be republished to the support
/// group with no way for them to know.
static ACTIVE_MEDIA_GROUP: LazyLock<Mutex<HashMap<i64, String>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

/// User tapped "📞 Goldaw" or sent /support — prompt them to type their message.
pub async fn handle_support_button(bot: &Bot, msg: &Message, lang: Lang) {
    let config = get_config();
    if config.support_chat_id.is_none() {
        let _ = bot
            .send_message(msg.chat.id, t("support_unavailable", lang))
            .await;
        return;
    }

    if let Some(user) = &msg.from {
        // Only one input mode at a time — see `handlers::clear_input_modes`.
        super::clear_input_modes(user.id.0 as i64).await;
        WAITING_FOR_SUPPORT.lock().await.insert(user.id.0 as i64);
    }

    let _ = bot
        .send_message(msg.chat.id, t("support_prompt", lang))
        .await;
}

/// Check if user is in support-waiting state.
pub async fn is_waiting_for_support(user_id: i64) -> bool {
    WAITING_FOR_SUPPORT.lock().await.contains(&user_id)
}

/// Cancel support-waiting state for a user.
pub async fn cancel_support(user_id: i64) {
    WAITING_FOR_SUPPORT.lock().await.remove(&user_id);
    ACTIVE_MEDIA_GROUP.lock().await.remove(&user_id);
}

/// Whether an incoming non-text message should be forwarded to support.
///
/// True when the user is in support mode, or when this is a later part of the album
/// whose first part we just accepted.
pub async fn should_accept_attachment(msg: &Message, user_id: i64) -> bool {
    if is_waiting_for_support(user_id).await {
        return true;
    }
    match msg.media_group_id() {
        Some(gid) => ACTIVE_MEDIA_GROUP
            .lock()
            .await
            .get(&user_id)
            .is_some_and(|active| active.as_str() == gid.0),
        None => false,
    }
}

/// Telegram caps a media caption at 1024 characters — a quarter of a text message.
/// The routing header has to live in the caption for media, so the user's own words are
/// what gets trimmed to fit.
const CAPTION_MAX: usize = 1024;
/// Cap for a plain text forward.
const TEXT_MAX: usize = 4096;

/// Build the forward exactly one way, for text and for media alike.
///
/// The header must come first and the `🆔` line must survive intact — `extract_user_id`
/// reads the FIRST marker line, and everything below it is untrusted user text. So the
/// body is the only part that may be trimmed, and it is trimmed on a char boundary.
fn build_forward(
    first_name: &str,
    username: &str,
    user_id: i64,
    body: &str,
    budget: usize,
) -> String {
    let head = format!(
        "{}\n\n👤 {} (@{})\n{} {}\n\n💬 ",
        FORWARD_HEADER,
        sanitize_header_field(first_name),
        sanitize_header_field(username),
        ID_MARKER,
        user_id
    );
    // `budget` is a Telegram limit, measured the way Telegram measures: UTF-16 units.
    let head_len = head.encode_utf16().count();
    let room = budget.saturating_sub(head_len + 1);

    format!("{}{}", head, trim_to_budget(body, room))
}

/// Trim `text` to `budget` UTF-16 units on a char boundary, marking the cut.
///
/// UTF-16 because that is the unit Telegram counts, and on a char boundary because
/// slicing an astral character in half yields a replacement glyph.
fn trim_to_budget(text: &str, budget: usize) -> String {
    if text.encode_utf16().count() <= budget {
        return text.to_string();
    }
    let mut out = String::new();
    let mut used = 0usize;
    for c in text.chars() {
        let w = c.len_utf16();
        // Leave room for the ellipsis that marks the cut.
        if used + w > budget.saturating_sub(1) {
            break;
        }
        out.push(c);
        used += w;
    }
    out.push('…');
    out
}

/// Receive the user's support message and forward to the support group.
pub async fn handle_support_message(bot: &Bot, msg: &Message, lang: Lang) {
    let config = get_config();
    let Some(support_chat_id) = config.support_chat_id else {
        return;
    };

    let Some(user) = &msg.from else { return };
    let user_id = user.id.0 as i64;

    // Remove from waiting set
    WAITING_FOR_SUPPORT.lock().await.remove(&user_id);

    let Some(message_text) = msg.text() else {
        let _ = bot
            .send_message(msg.chat.id, t("support_text_only", lang))
            .await;
        return;
    };

    // Admin-facing message stays as-is (not localized). `message_text` is deliberately
    // NOT sanitized: it is rendered below the 🆔 line and `extract_user_id` reads only
    // the first marker line, so it cannot affect routing.
    let forward_text = build_forward(
        user.first_name.as_str(),
        user.username.as_deref().unwrap_or("N/A"),
        user_id,
        message_text,
        TEXT_MAX,
    );

    let _ = bot
        .send_message(ChatId(support_chat_id), &forward_text)
        .await;
    let _ = bot.send_message(msg.chat.id, t("support_sent", lang)).await;
}

/// What kind of file the user sent, and the `file_id` to re-send it by.
///
/// Re-sending by `file_id` means Telegram just re-references the stored file: no
/// download, no upload, and no size limit for us to hit.
enum Attachment {
    Photo(String),
    Document(String),
    Video(String),
    Voice(String),
    Audio(String),
    Animation(String),
}

/// Classify an incoming message as a supported attachment.
///
/// Stickers, locations, contacts and polls are deliberately absent — they are not
/// useful support evidence, and forwarding them would just be noise in the group.
fn detect_attachment(msg: &Message) -> Option<Attachment> {
    if let Some(sizes) = msg.photo() {
        // Photos arrive as a ladder of sizes; the last is the largest.
        return sizes
            .last()
            .map(|p| Attachment::Photo(p.file.id.to_string()));
    }
    if let Some(d) = msg.document() {
        return Some(Attachment::Document(d.file.id.to_string()));
    }
    if let Some(v) = msg.video() {
        return Some(Attachment::Video(v.file.id.to_string()));
    }
    if let Some(v) = msg.voice() {
        return Some(Attachment::Voice(v.file.id.to_string()));
    }
    if let Some(a) = msg.audio() {
        return Some(Attachment::Audio(a.file.id.to_string()));
    }
    if let Some(a) = msg.animation() {
        return Some(Attachment::Animation(a.file.id.to_string()));
    }
    None
}

/// Send one attachment to a chat, re-referencing it by `file_id`.
///
/// Shared by both directions: a user's file going up to the support group, and an
/// agent's file coming back down.
async fn send_attachment(
    bot: &Bot,
    chat: ChatId,
    att: Attachment,
    caption: String,
) -> Result<(), teloxide::RequestError> {
    match att {
        Attachment::Photo(id) => bot
            .send_photo(chat, InputFile::file_id(id.into()))
            .caption(caption)
            .await
            .map(|_| ()),
        Attachment::Document(id) => bot
            .send_document(chat, InputFile::file_id(id.into()))
            .caption(caption)
            .await
            .map(|_| ()),
        Attachment::Video(id) => bot
            .send_video(chat, InputFile::file_id(id.into()))
            .caption(caption)
            .await
            .map(|_| ()),
        Attachment::Voice(id) => bot
            .send_voice(chat, InputFile::file_id(id.into()))
            .caption(caption)
            .await
            .map(|_| ()),
        Attachment::Audio(id) => bot
            .send_audio(chat, InputFile::file_id(id.into()))
            .caption(caption)
            .await
            .map(|_| ()),
        Attachment::Animation(id) => bot
            .send_animation(chat, InputFile::file_id(id.into()))
            .caption(caption)
            .await
            .map(|_| ()),
    }
}

/// Forward a user's screenshot or file to the support group.
///
/// Re-sent as the bot rather than `forwardMessage`: a native forward keeps the USER as
/// author, and `handle_support_reply` requires the replied-to message to be authored by
/// this bot. A forward would be rejected by our own guard, so an agent could not answer
/// it. Re-sending keeps the identity check and the reply routing intact.
pub async fn handle_support_attachment(bot: &Bot, msg: &Message, lang: Lang) {
    let config = get_config();
    let Some(support_chat_id) = config.support_chat_id else {
        return;
    };
    // Private chats only. Without this, a support agent who had ever armed support mode
    // in DM would have every photo they post IN THE SUPPORT GROUP re-sent by the bot
    // straight back into that same group.
    if !msg.chat.is_private() {
        return;
    }

    let Some(user) = &msg.from else { return };
    let user_id = user.id.0 as i64;

    let Some(att) = detect_attachment(msg) else {
        // Something we do not forward (a sticker, a location). Leave the user in
        // support mode so they can try again rather than silently dropping them.
        let _ = bot
            .send_message(msg.chat.id, t("support_unsupported_file", lang))
            .await;
        return;
    };

    // Support mode is one-shot, exactly like the text path. An album's remaining parts
    // are admitted by `should_accept_attachment` on the media_group_id recorded here,
    // NOT by leaving the flag set — leaving it set stranded the user in support mode
    // with no inline callback able to clear it.
    WAITING_FOR_SUPPORT.lock().await.remove(&user_id);

    // Whether this is a later part of an album we have already acknowledged. Telegram
    // delivers an album as one update per photo, so without this a three-photo album
    // answered "your message was sent, we will reply soon" three times over.
    // Check-and-insert under one lock so the two cannot disagree.
    let album_continuation = match msg.media_group_id() {
        Some(gid) => {
            let mut active = ACTIVE_MEDIA_GROUP.lock().await;
            let seen = active.get(&user_id).is_some_and(|a| a.as_str() == gid.0);
            active.insert(user_id, gid.0.clone());
            seen
        }
        None => false,
    };

    // The header must fit in a CAPTION, which Telegram caps far lower than a message.
    let caption = build_forward(
        user.first_name.as_str(),
        user.username.as_deref().unwrap_or("N/A"),
        user_id,
        msg.caption().unwrap_or(""),
        CAPTION_MAX,
    );

    let sent = send_attachment(bot, ChatId(support_chat_id), att, caption).await;

    // Unlike most sends in this file, this one is checked: if the file never reached
    // support, telling the user "sent" would be a lie, and they would sit waiting for
    // an answer to a message nobody received.
    match sent {
        Ok(()) => {
            // `support_sent` — the same one the text path uses — because this path is
            // one-shot too. An earlier revision left support mode open and said "you can
            // send more, or type your message"; when support mode went back to one-shot
            // that message became false in both halves, and both failed silently: a
            // second standalone photo has no media_group_id so `should_accept_attachment`
            // rejects it, and typed text falls through to `admin::handle_admin_text_input`,
            // a no-op for non-admins. The user was invited to do two things that would
            // each vanish without a word.
            //
            // A caption carries the user's explanation with the file, so one action is
            // genuinely enough; to add more they tap Support again.
            if !album_continuation {
                let _ = bot.send_message(msg.chat.id, t("support_sent", lang)).await;
            }
        }
        Err(e) => {
            // Always reported, even mid-album: a silently missing photo out of three is
            // exactly the failure the user cannot see for themselves.
            eprintln!(
                "Failed to forward a support attachment from TG:{}: {}",
                user_id, e
            );
            let _ = bot
                .send_message(msg.chat.id, t("support_send_failed", lang))
                .await;
        }
    }
}

/// Handle admin replies in the support group: extract user ID and send reply to user.
pub async fn handle_support_reply(bot: &Bot, db: &DatabaseConnection, msg: &Message) {
    let Some(reply) = msg.reply_to_message() else {
        return;
    };

    // Only handle replies to a message THIS bot authored.
    //
    // `is_bot` alone was not enough, and the gap was an authorization hole rather than a
    // nit: it is true for *any* bot, so a second bot in the support group that echoes
    // user-supplied text (anti-spam, welcome, logging — routine) turns attacker-chosen
    // text into a valid reply target. Telegram also fills `from` with a *fake* bot
    // sender for messages sent on behalf of a chat, so an anonymous admin's post or a
    // channel post clears an `is_bot` check too. Either way a group member could put
    // `🆔 <any id>` in text the bot never wrote and have a support reply delivered to a
    // stranger as an official message.
    //
    // `sender_chat` is rejected for the on-behalf-of-a-chat case; the id comparison
    // pins the author to us specifically.
    if reply.sender_chat.is_some() {
        return;
    }
    let Some(me) = bot_user_id(bot).await else {
        eprintln!("Support reply: could not resolve own bot id; ignoring");
        // Say so. Failing closed silently is the invisible-failure class this codebase
        // has been bitten by: the agent types a reply, nothing happens, and the user
        // never hears back. Constant text, no marker line, so it cannot be replied to
        // as if it were a forward.
        let _ = bot
            .send_message(msg.chat.id, "⚠️ Jogap iberilmedi — gaýtadan synanyşyň.")
            .await;
        return;
    };
    if reply.from.as_ref().map(|u| u.id) != Some(me) {
        return;
    }

    // A forwarded attachment carries the routing header in its CAPTION, not its text,
    // so an agent replying to a screenshot would otherwise never route.
    let Some(original_text) = reply.text().or_else(|| reply.caption()) else {
        return;
    };

    // Only a support forward carries a routable id. See FORWARD_HEADER.
    if !original_text.starts_with(FORWARD_HEADER) {
        return;
    }

    let Some(user_id) = extract_user_id(original_text) else {
        return;
    };

    // Get the user's language to send the reply in their preferred language
    let user_lang = get_user_language(db, user_id).await.unwrap_or_default();

    // An agent answering a screenshot with their own screenshot is the point of a
    // support conversation that carries images. Without this the handler returned before
    // BOTH the send and the acknowledgement below: the agent saw no error, believed they
    // had answered, and the user received nothing. This change makes that far more
    // likely, since support threads now contain images by design.
    if let Some(att) = detect_attachment(msg) {
        let body = tf(
            "support_reply",
            user_lang,
            &[("message", msg.caption().unwrap_or(""))],
        );
        match send_attachment(
            bot,
            ChatId(user_id),
            att,
            trim_to_budget(&body, CAPTION_MAX),
        )
        .await
        {
            Ok(()) => {
                let _ = bot.send_message(msg.chat.id, "✅ Jogap iberildi.").await;
            }
            Err(e) => {
                eprintln!(
                    "Failed to deliver an agent attachment to TG:{}: {}",
                    user_id, e
                );
                let _ = bot
                    .send_message(msg.chat.id, "❌ Jogap iberilmedi — gaýtadan synanyşyň.")
                    .await;
            }
        }
        return;
    }

    let Some(reply_text) = msg.text() else {
        // A sticker, a poll — something we cannot deliver. Say so in the group rather
        // than returning silently and letting the agent believe the user was answered.
        let _ = bot
            .send_message(msg.chat.id, "⚠️ Bu görnüşli jogap iberilmeýär.")
            .await;
        return;
    };

    let _ = bot
        .send_message(
            ChatId(user_id),
            tf("support_reply", user_lang, &[("message", reply_text)]),
        )
        .await;

    let _ = bot.send_message(msg.chat.id, "✅ Jogap iberildi.").await;
}

/// First line of the support forward, and the proof that a message IS one.
///
/// `ID_MARKER` is NOT unique to this message: six locale strings (`payment_created` and
/// `balance_topped_up`, in all three languages) also render a line starting with it, and
/// they CAN be emitted into the support group — the "waiting for input" states are keyed
/// by user id, not chat id, so a group member can arm one in DM and trigger it in the
/// group. Those messages are authored by this bot, so they pass the identity check, and
/// today they survive only because the text after the marker is a label rather than
/// digits. That is luck, not design: NOWPayments payment ids are numeric, so a locale
/// edit to `🆔 {payment_id}` would turn one into a live reply-hijack.
///
/// Requiring the forward header removes the coincidence from the security argument.
const FORWARD_HEADER: &str = "📩 Täze goldaw habary";
const ID_MARKER: &str = "🆔";

/// This bot's own `UserId`, fetched once and cached.
///
/// `handle_support_reply` needs it to prove a replied-to message was authored by *us*
/// and not merely by "a bot". Cached because that check runs on every reply in the
/// support group and the answer never changes for a given token; a failed lookup is not
/// cached, so a transient API error is retried on the next reply.
static BOT_USER_ID: LazyLock<Mutex<Option<UserId>>> = LazyLock::new(|| Mutex::new(None));

async fn bot_user_id(bot: &Bot) -> Option<UserId> {
    let mut cached = BOT_USER_ID.lock().await;
    if let Some(id) = *cached {
        return Some(id);
    }
    match bot.get_me().await {
        Ok(me) => {
            let id = me.user.id;
            *cached = Some(id);
            Some(id)
        }
        Err(e) => {
            eprintln!("Failed to resolve own bot id: {}", e);
            None
        }
    }
}

/// Remove anything from a user-controlled header field that could forge the
/// `🆔 <id>` line `extract_user_id` reads back.
///
/// A newline would let a display name open a line of its own, and the marker itself
/// would let that line start with `🆔 `. Together those were enough to redirect a
/// support agent's reply: `first_name` is rendered *above* the real id, so a display
/// name of `🆔 999999999` won the old first-match `text.find()` and the reply was
/// delivered to an arbitrary account as an official support message.
fn sanitize_header_field(s: &str) -> String {
    s.chars()
        .filter(|c| !matches!(c, '\n' | '\r' | '🆔'))
        .collect()
}

/// Extract the user id from the bot's own forwarded support message.
///
/// Commits to the FIRST line that *begins* with the marker, and returns `None` if that
/// line does not parse. Every part of that is load-bearing: anchoring to the start of a
/// line stops a field rendered above it from injecting one (belt-and-braces with
/// `sanitize_header_field`); taking the first match stops the freeform message body —
/// rendered below the id and deliberately not sanitized — from overriding it; and
/// committing rather than searching on means an unparseable first marker line fails
/// closed instead of letting a later, attacker-chosen line win.
fn extract_user_id(text: &str) -> Option<i64> {
    let marker = format!("{} ", ID_MARKER);
    let marker = marker.as_str();
    text.lines()
        .find_map(|line| line.strip_prefix(marker))
        .and_then(|rest| {
            let end = rest
                .find(|c: char| !c.is_ascii_digit())
                .unwrap_or(rest.len());
            rest[..end].parse::<i64>().ok()
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The real builder, so these tests break if the forward format is ever reordered.
    fn forward(first_name: &str, username: &str, user_id: i64, message: &str) -> String {
        build_forward(first_name, username, user_id, message, TEXT_MAX)
    }

    #[test]
    fn extracts_the_real_id_from_a_normal_forward() {
        assert_eq!(
            extract_user_id(&forward("Bob", "bob", 111, "hi")),
            Some(111)
        );
    }

    /// A display name of "🆔 999999999" used to win the old first-match `text.find()`,
    /// so a support agent's reply was delivered to an arbitrary account.
    #[test]
    fn a_display_name_cannot_hijack_the_reply_target() {
        let evil = forward("🆔 999999999", "victim", 111, "hello");
        assert_eq!(extract_user_id(&evil), Some(111));
    }

    /// Newlines in a display name must not be able to open a forged marker line.
    #[test]
    fn a_newline_in_a_display_name_cannot_forge_a_marker_line() {
        let evil = forward("x\n🆔 999999999", "victim", 111, "hello");
        assert_eq!(extract_user_id(&evil), Some(111));
    }

    /// The message body is rendered BELOW the id and is deliberately not sanitized,
    /// so the parse must take the first marker line, not the last.
    #[test]
    fn the_message_body_cannot_override_the_reply_target() {
        let evil = forward("Bob", "bob", 111, "please help\n🆔 999999999");
        assert_eq!(extract_user_id(&evil), Some(111));
    }

    /// `find_map(parse)` used to skip a marker line that failed to parse and let a
    /// LATER one win — and the body below the id is deliberately unsanitized, so that
    /// was one format change away from being exploitable. Committing to the first
    /// marker line must fail closed instead.
    #[test]
    fn an_unparseable_first_marker_line_fails_closed() {
        assert_eq!(extract_user_id("🆔 \n🆔 999999999"), None);
        assert_eq!(
            extract_user_id("🆔 18446744073709551615\n🆔 999999999"),
            None
        );
    }

    #[test]
    fn sanitizer_strips_exactly_what_str_lines_splits_on() {
        // `str::lines()` splits on \n and \r\n only — verified by execution — so those
        // are the separators a header field must not be able to smuggle in.
        assert_eq!(sanitize_header_field("a\nb\r\nc"), "abc");
        assert_eq!(sanitize_header_field("🆔 123"), " 123");
        // Characters `lines()` does NOT split on may pass through untouched.
        for sep in ["\u{000B}", "\u{000C}", "\u{0085}", "\u{2028}", "\u{2029}"] {
            let payload = format!("x{sep}🆔 999999999");
            assert_eq!(
                extract_user_id(&forward(&payload, "victim", 111, "hi")),
                Some(111),
                "separator {:?} must not create a marker line",
                sep
            );
        }
    }

    /// `🆔 ` is not unique to the support forward: `payment_created` and
    /// `balance_topped_up` render one too, in all three locales, and both can be emitted
    /// INTO the support group because the input states are keyed by user id rather than
    /// chat id. They are authored by this bot, so they clear the identity check. Today
    /// they fail to parse only because the text after the marker is a label — which is a
    /// coincidence, not a guarantee, since payment ids are numeric.
    #[test]
    fn a_payment_message_is_not_mistaken_for_a_support_forward() {
        // As rendered, for each locale, with a numeric id where the label sits — i.e.
        // the shape one careless locale edit away.
        for label in [
            "🆔 Töleg ID:",
            "🆔 ID платежа:",
            "🆔 Payment ID:",
            "🆔 4500334452",
        ] {
            let payment_msg = format!("✅ Töleg döredildi\n\n{}\n4500334452\n", label);
            assert!(
                !payment_msg.starts_with(FORWARD_HEADER),
                "{:?} must not be treated as a support forward",
                label
            );
        }
        // And the real forward still is one.
        assert!(forward("Bob", "bob", 111, "hi").starts_with(FORWARD_HEADER));
    }

    /// A media caption is capped at 1024 units, a quarter of a text message. The
    /// routing header has to survive that budget or an agent's reply cannot be routed
    /// back to the sender at all — so only the user's own words may be trimmed.
    #[test]
    fn a_caption_stays_within_telegrams_limit_and_keeps_the_header() {
        for body_len in [0usize, 10, 900, 1024, 5000] {
            let body = "ä".repeat(body_len); // 2-byte char, 1 UTF-16 unit
            let cap = build_forward("Bob", "bob", 1317186982, &body, CAPTION_MAX);
            let units = cap.encode_utf16().count();
            assert!(units <= CAPTION_MAX, "body_len={body_len} -> {units} units");
            assert!(
                cap.starts_with(FORWARD_HEADER),
                "header lost at body_len={body_len}"
            );
            assert_eq!(
                extract_user_id(&cap),
                Some(1317186982),
                "routing broken at body_len={body_len}"
            );
        }
    }

    /// Trimming must not cut a character in half, and must be visibly a trim.
    #[test]
    fn an_overlong_caption_is_trimmed_on_a_char_boundary() {
        let body = "😀".repeat(2000); // 2 UTF-16 units each
        let cap = build_forward("Bob", "bob", 111, &body, CAPTION_MAX);
        assert!(cap.encode_utf16().count() <= CAPTION_MAX);
        assert!(
            cap.ends_with('…'),
            "a trimmed caption should show it was trimmed"
        );
        // Valid UTF-8 with no replacement characters => no split surrogate pair.
        assert!(!cap.contains('\u{FFFD}'));
        assert_eq!(extract_user_id(&cap), Some(111));
    }

    /// A caption is attacker-controlled and sits BELOW the id, exactly like message text,
    /// so the same first-match rule must protect it.
    #[test]
    fn a_caption_cannot_hijack_the_reply_target() {
        let cap = build_forward("Bob", "bob", 111, "look\n🆔 999999999", CAPTION_MAX);
        assert_eq!(extract_user_id(&cap), Some(111));
        let cap2 = build_forward("🆔 999999999", "victim", 111, "hi", CAPTION_MAX);
        assert_eq!(extract_user_id(&cap2), Some(111));
    }

    /// Text forwards keep the larger budget, so ordinary messages are never trimmed.
    #[test]
    fn a_normal_text_message_is_never_trimmed() {
        let body = "please help me with my subscription";
        let out = forward("Bob", "bob", 111, body);
        assert!(out.ends_with(body));
        assert!(!out.ends_with('…'));
    }

    /// The trim is shared by the forward builder and the agent-reply path, so its
    /// contract is worth pinning directly: never exceed the budget, never split a
    /// character, and always show that a cut happened.
    #[test]
    fn trim_to_budget_never_exceeds_and_never_splits_a_char() {
        for (text, budget) in [
            ("short", 100usize),
            ("exactly-ten", 11),
            (&"😀".repeat(500), 50),
            (&"ä".repeat(500), 50),
            (&"a".repeat(500), 1),
            ("", 0),
        ] {
            let out = trim_to_budget(text, budget);
            let units = out.encode_utf16().count();
            assert!(
                units <= budget.max(1),
                "{:?} @ {} -> {} units",
                &text[..text.len().min(20)],
                budget,
                units
            );
            assert!(!out.contains('\u{FFFD}'), "split a character");
        }
        // Untouched when it already fits, and marked when it does not.
        assert_eq!(trim_to_budget("fits", 10), "fits");
        assert!(trim_to_budget(&"x".repeat(100), 10).ends_with('…'));
    }

    #[test]
    fn no_marker_means_no_target() {
        assert_eq!(extract_user_id("just some chatter"), None);
        assert_eq!(extract_user_id(""), None);
    }
}
