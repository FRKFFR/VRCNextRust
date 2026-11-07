//! VRChat chatbox OSC messages (`/chatbox/input`, `/chatbox/typing`).
//!
//! Community-documented behavior: the chatbox accepts at most 144
//! characters; control characters are meaningless to it and are stripped.
//! All clamping happens on `char` boundaries — never on byte offsets, so
//! multi-byte UTF-8 is never split.

use rosc::{OscMessage, OscType};

/// Maximum number of characters VRChat displays in the chatbox.
pub const CHATBOX_MAX_CHARS: usize = 144;

/// OSC address for chatbox text input.
pub const CHATBOX_INPUT_ADDR: &str = "/chatbox/input";
/// OSC address for the chatbox typing indicator.
pub const CHATBOX_TYPING_ADDR: &str = "/chatbox/typing";

/// Sanitize chatbox text: drop control characters, then clamp to
/// [`CHATBOX_MAX_CHARS`] `char`s (character boundaries, not bytes).
pub fn sanitize_chatbox(text: &str) -> String {
    text.chars()
        .filter(|c| !c.is_control())
        .take(CHATBOX_MAX_CHARS)
        .collect()
}

/// Build the `/chatbox/input` message.
///
/// `immediate = true` sends the text at once; `false` lets VRChat type it
/// out character by character. Text is sanitized via [`sanitize_chatbox`].
pub fn chatbox_input(text: &str, immediate: bool) -> OscMessage {
    OscMessage {
        addr: CHATBOX_INPUT_ADDR.to_string(),
        args: vec![
            OscType::String(sanitize_chatbox(text)),
            OscType::Bool(immediate),
        ],
    }
}

/// Build the `/chatbox/typing` message — toggles the "typing…" indicator.
pub fn chatbox_typing(typing: bool) -> OscMessage {
    OscMessage {
        addr: CHATBOX_TYPING_ADDR.to_string(),
        args: vec![OscType::Bool(typing)],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_control_characters() {
        assert_eq!(sanitize_chatbox("a\nb\tc\rd\u{7}e"), "abcde");
        // DEL and other C0/C1 controls are stripped too.
        assert_eq!(sanitize_chatbox("x\u{0}y\u{1f}z"), "xyz");
    }

    #[test]
    fn clamps_to_144_chars() {
        let long = "a".repeat(200);
        assert_eq!(sanitize_chatbox(&long).chars().count(), CHATBOX_MAX_CHARS);
    }

    #[test]
    fn clamps_on_char_boundaries() {
        // 143 ASCII + one 4-byte emoji + more text: the emoji must either
        // fit whole or be dropped — never split into invalid UTF-8.
        let text = format!("{}🦀tail", "a".repeat(143));
        let out = sanitize_chatbox(&text);
        assert!(out.is_char_boundary(out.len()));
        assert_eq!(out.chars().count(), CHATBOX_MAX_CHARS);
        assert!(out.ends_with('🦀'));

        // A string that would split mid-codepoint still yields valid UTF-8.
        let emoji_heavy = "🦀".repeat(200);
        let out = sanitize_chatbox(&emoji_heavy);
        assert_eq!(out.chars().count(), CHATBOX_MAX_CHARS);
        assert!(out.chars().all(|c| c == '🦀'));
    }

    #[test]
    fn chatbox_input_message_shape() {
        let msg = chatbox_input("  hi\nthere  ", true);
        assert_eq!(msg.addr, "/chatbox/input");
        assert_eq!(
            msg.args,
            vec![
                OscType::String("  hithere  ".to_string()),
                OscType::Bool(true)
            ]
        );
        let msg = chatbox_input("hi", false);
        assert_eq!(msg.args[1], OscType::Bool(false));
    }

    #[test]
    fn typing_message_shape() {
        let msg = chatbox_typing(true);
        assert_eq!(msg.addr, "/chatbox/typing");
        assert_eq!(msg.args, vec![OscType::Bool(true)]);
    }
}
