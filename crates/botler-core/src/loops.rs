//! Loop defences between agents. Pure: the hub feeds in texts, times and turn activity.
//!
//! Three layers, cheapest first:
//! 1. [`TurnLog`]: a child's end-of-turn notice to its parent is skipped when the last thing the
//!    child did was messaging that parent (the parent already has the news).
//! 2. [`is_acknowledgement`] / [`wakes`]: short closing messages ("thanks, no reply needed",
//!    "got it 👍") are delivered as notes that never start a turn, so politeness cannot ping-pong.
//! 3. [`pair_overflow`]: too many agent-to-agent exchanges between one pair without the user or
//!    the outside world in between trips a breaker for that pair.
//!
//! The correlation-chain hop limit ([`crate::limits::MAX_CHAIN_HOPS`]) stays as a backstop.

use chrono::{DateTime, Duration, Utc};

/// The words an acknowledgement is made of. A message counts as one only if every word is in
/// here and it has at least one [`ACK_CORE`] word (or phrase), so anything with content in it
/// ("ok, but the tests fail") still wakes the recipient.
const ACK_FILLER: &[&str] = &[
    "a",
    "again",
    "all",
    "and",
    "best",
    "both",
    "bye",
    "clear",
    "end",
    "everything",
    "for",
    "from",
    "further",
    "glad",
    "good",
    "has",
    "have",
    "hear",
    "help",
    "here",
    "i",
    "ill",
    "im",
    "info",
    "information",
    "is",
    "it",
    "its",
    "loud",
    "lot",
    "makes",
    "me",
    "message",
    "more",
    "much",
    "my",
    "necessary",
    "need",
    "needed",
    "no",
    "now",
    "of",
    "on",
    "regards",
    "reply",
    "required",
    "response",
    "same",
    "sense",
    "set",
    "side",
    "so",
    "that",
    "thats",
    "the",
    "then",
    "this",
    "to",
    "too",
    "update",
    "very",
    "we",
    "well",
    "will",
    "worries",
    "you",
    "your",
    "youre",
    "action",
    "do",
    "problem",
    "really",
    "everyone",
    "yes",
];

/// Words that make a message an acknowledgement on their own.
const ACK_CORE: &[&str] = &[
    "ack",
    "acknowledged",
    "alright",
    "appreciate",
    "appreciated",
    "awesome",
    "cheers",
    "cool",
    "copy",
    "excellent",
    "got",
    "great",
    "k",
    "kk",
    "nice",
    "noted",
    "np",
    "ok",
    "okay",
    "perfect",
    "received",
    "roger",
    "sounds",
    "sure",
    "thank",
    "thanks",
    "thx",
    "ty",
    "understood",
    "welcome",
    "wilco",
];

/// Phrases of filler words that also close a conversation.
const ACK_PHRASES: &[&str] = &[
    "no reply",
    "no response",
    "no need",
    "no action",
    "no problem",
    "no worries",
    "will do",
    "makes sense",
    "all good",
    "all set",
];

/// Emoji that mean "ok".
const ACK_EMOJI: &[char] = &['👍', '👌', '🙏', '✅', '🙂', '😊', '🎉', '💯'];

/// Longest text still treated as a possible acknowledgement.
const ACK_MAX_CHARS: usize = 120;
const ACK_MAX_WORDS: usize = 14;

/// Whether `text` only acknowledges or closes a conversation and carries nothing to act on.
/// Deterministic and deliberately conservative: a question mark, a long text or any word outside
/// the acknowledgement vocabulary makes it a real message.
pub fn is_acknowledgement(text: &str) -> bool {
    let text = text.trim();
    if text.is_empty() || text.chars().count() > ACK_MAX_CHARS || text.contains('?') {
        return false;
    }
    let has_emoji = text.chars().any(|c| ACK_EMOJI.contains(&c));
    let lower = text.to_lowercase().replace(['\u{2019}', '\''], "");
    let words: Vec<&str> = lower.split(|c: char| !c.is_alphanumeric()).filter(|w| !w.is_empty()).collect();
    if words.len() > ACK_MAX_WORDS {
        return false;
    }
    // Anything that is not a word must be punctuation, spacing or an ok-emoji.
    let stray = lower.chars().any(|c| {
        !(c.is_alphanumeric()
            || c.is_whitespace()
            || ".,!:;-–—()".contains(c)
            || ACK_EMOJI.contains(&c)
            || c == '\u{fe0f}')
    });
    if stray || !words.iter().all(|w| ACK_CORE.contains(w) || ACK_FILLER.contains(w)) {
        return false;
    }
    let joined = words.join(" ");
    has_emoji || words.iter().any(|w| ACK_CORE.contains(w)) || ACK_PHRASES.iter().any(|p| joined.contains(p))
}

/// Whether an agent-to-agent message should wake its recipient. An acknowledgement does not,
/// unless it answers a question: if the recipient's last message to the sender asked something,
/// even "ok" is the answer it is waiting for.
pub fn wakes(text: &str, recipients_last_message_to_sender: Option<&str>) -> bool {
    !is_acknowledgement(text) || recipients_last_message_to_sender.is_some_and(|m| m.contains('?'))
}

/// The breaker subject for a pair of agents, the same in both directions.
pub fn pair_subject(a: &str, b: &str) -> String {
    let (x, y) = if a <= b { (a, b) } else { (b, a) };
    format!("pair:{x}|{y}")
}

/// The two instances of a pair breaker subject.
pub fn pair_members(subject: &str) -> Option<(&str, &str)> {
    subject.strip_prefix("pair:")?.split_once('|')
}

/// How many agent-to-agent exchanges one pair may have within `window` while neither the user
/// nor anything external reached either of them.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PairLimit {
    pub max_exchanges: usize,
    pub window: Duration,
}

impl Default for PairLimit {
    fn default() -> Self {
        Self { max_exchanges: 8, window: Duration::minutes(10) }
    }
}

/// Whether the exchanges between a pair (times of agent-to-agent deliveries in either
/// direction, including the one being routed) exceed `limit`. Exchanges at or before `reset`
/// (the last user or external event to either agent, or the user clearing the breaker) do not
/// count.
pub fn pair_overflow(
    exchanges: &[DateTime<Utc>],
    reset: Option<DateTime<Utc>>,
    now: DateTime<Utc>,
    limit: PairLimit,
) -> bool {
    let since = now - limit.window;
    exchanges.iter().filter(|t| **t >= since && reset.is_none_or(|r| **t > r)).count() > limit.max_exchanges
}

/// What an agent did during its current turn, in order: tool starts and the hub messages it
/// sent. Enough to tell whether the turn ended by messaging someone.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct TurnLog {
    seq: u64,
    last_tool: u64,
    last_message: Option<(u64, String)>,
}

impl TurnLog {
    /// A new turn begins.
    pub fn reset(&mut self) {
        *self = Self::default();
    }

    /// pi started a tool. The tool that sends a hub message is not counted: its start is
    /// reported alongside the message itself, and only what happens after it matters.
    pub fn tool_started(&mut self, tool: &str) {
        if tool.ends_with("send_message") {
            return;
        }
        self.seq += 1;
        self.last_tool = self.seq;
    }

    /// The agent sent a message to `to` through the hub.
    pub fn messaged(&mut self, to: &str) {
        self.seq += 1;
        self.last_message = Some((self.seq, to.to_string()));
    }

    /// Whether the last thing the agent did this turn was messaging `whom`.
    pub fn ended_by_messaging(&self, whom: &str) -> bool {
        self.last_message.as_ref().is_some_and(|(seq, to)| to == whom && *seq > self.last_tool)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn acknowledgements_are_recognised() {
        for t in [
            "thanks",
            "Thanks!",
            "Thank you, no reply needed.",
            "thanks, no reply needed",
            "Acknowledged.",
            "👍",
            "ok",
            "OK 👍",
            "Got it.",
            "got it, thanks!",
            "Sounds good!",
            "Will do.",
            "Noted, thanks for the update.",
            "Perfect, thank you very much!",
            "No problem.",
            "You're welcome!",
            "Great, all set.",
            "Understood — no further action needed.",
            "Makes sense.",
            "Cheers",
            "✅ received",
        ] {
            assert!(is_acknowledgement(t), "{t:?} should be an acknowledgement");
        }
    }

    #[test]
    fn real_messages_are_not() {
        for t in [
            "ok, but what about X?",
            "ok, but the tests fail",
            "Done.",
            "done: built the parser on agents/dev-1/work",
            "No.",
            "no, that's wrong",
            "Thanks! I'll look into the failing job tomorrow.",
            "Can you check the logs",
            "Got it, starting on the scheduler now.",
            "thanks, here is the link: https://example.com",
            "",
            "Thanks for the update. The deployment failed on the gate with three pyright errors in the poller, \
             please fix those first.",
            "ok `rm -rf` it",
            "Reply needed",
        ] {
            assert!(!is_acknowledgement(t), "{t:?} should wake");
        }
    }

    #[test]
    fn an_answer_to_a_question_wakes_even_if_short() {
        assert!(!wakes("ok", None));
        assert!(!wakes("ok", Some("Done, merged.")));
        assert!(wakes("ok", Some("Shall I merge it now?")));
        assert!(wakes("the tests fail", None));
    }

    #[test]
    fn pair_subject_is_symmetric() {
        assert_eq!(pair_subject("dev-2", "botler"), pair_subject("botler", "dev-2"));
        assert_eq!(pair_members(&pair_subject("b", "a")), Some(("a", "b")));
        assert_eq!(pair_members("instance:a"), None);
    }

    #[test]
    fn pair_overflow_counts_only_the_window_after_the_last_reset() {
        let now = Utc::now();
        let limit = PairLimit::default();
        let burst: Vec<_> = (0..9).map(|i| now - Duration::seconds(i * 10)).collect();
        assert!(pair_overflow(&burst, None, now, limit));
        assert!(!pair_overflow(&burst[..8], None, now, limit), "8 is fine");
        // The user talked to one of them halfway through: only the later exchanges count.
        assert!(!pair_overflow(&burst, Some(now - Duration::seconds(45)), now, limit));
        // Old exchanges fall out of the window.
        let slow: Vec<_> = (0..9).map(|i| now - Duration::minutes(i * 2)).collect();
        assert!(!pair_overflow(&slow, None, now, limit));
    }

    #[test]
    fn turn_ending_with_a_message_to_the_parent() {
        let mut t = TurnLog::default();
        assert!(!t.ended_by_messaging("p"));
        t.tool_started("bash");
        t.tool_started("mcp__botler__send_message");
        t.messaged("p");
        assert!(t.ended_by_messaging("p"));
        assert!(!t.ended_by_messaging("other"));
        // More work after the message: the parent hears about the end of the turn again.
        t.tool_started("bash");
        assert!(!t.ended_by_messaging("p"));
        // A message to someone else last does not count either.
        t.messaged("p");
        t.messaged("q");
        assert!(!t.ended_by_messaging("p"));
        t.reset();
        assert_eq!(t, TurnLog::default());
    }
}
