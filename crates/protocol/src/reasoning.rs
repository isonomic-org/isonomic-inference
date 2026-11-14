//! The single table that maps reasoning effort between formats.

use crate::conversation::Effort;

/// One row of [`EFFORT_TABLE`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EffortRow {
    pub effort: Effort,
    /// The chat format's `reasoning_effort` string.
    pub chat: &'static str,
    /// The messages format's `output_config.effort`, or `None` when
    /// reasoning is turned off instead.
    pub messages: Option<&'static str>,
    /// Whether `messages` is an exact match or the nearest level.
    pub messages_exact: bool,
    /// The token budget this level stands for, where a format speaks in
    /// budgets rather than levels. The upper bound of the level's range.
    pub budget_tokens: u32,
}

/// Effort levels, lowest first.
pub const EFFORT_TABLE: [EffortRow; 6] = [
    row(Effort::None, "none", None, true, 0),
    row(Effort::Minimal, "minimal", Some("low"), false, 1024),
    row(Effort::Low, "low", Some("low"), true, 4096),
    row(Effort::Medium, "medium", Some("medium"), true, 8192),
    row(Effort::High, "high", Some("high"), true, 16384),
    row(Effort::Max, "xhigh", Some("high"), false, 32768),
];

const fn row(
    effort: Effort,
    chat: &'static str,
    messages: Option<&'static str>,
    messages_exact: bool,
    budget_tokens: u32,
) -> EffortRow {
    EffortRow {
        effort,
        chat,
        messages,
        messages_exact,
        budget_tokens,
    }
}

impl Effort {
    pub fn row(self) -> &'static EffortRow {
        EFFORT_TABLE
            .iter()
            .find(|r| r.effort == self)
            .expect("every effort has a row")
    }

    /// The lowest level whose budget covers `budget` tokens.
    pub fn from_budget(budget: u32) -> Effort {
        EFFORT_TABLE
            .iter()
            .find(|r| budget <= r.budget_tokens)
            .map_or(Effort::Max, |r| r.effort)
    }

    /// The level a chat-format effort string names.
    pub fn from_chat(value: &str) -> Option<Effort> {
        EFFORT_TABLE
            .iter()
            .find(|r| r.chat == value)
            .map(|r| r.effort)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rows_are_ordered_and_budgets_grow() {
        for pair in EFFORT_TABLE.windows(2) {
            assert!(pair[0].effort < pair[1].effort);
            assert!(pair[0].budget_tokens < pair[1].budget_tokens);
        }
    }

    #[test]
    fn budgets_map_to_the_lowest_covering_level() {
        assert_eq!(Effort::from_budget(0), Effort::None);
        assert_eq!(Effort::from_budget(1), Effort::Minimal);
        assert_eq!(Effort::from_budget(4096), Effort::Low);
        assert_eq!(Effort::from_budget(4097), Effort::Medium);
        assert_eq!(Effort::from_budget(1 << 30), Effort::Max);
    }

    #[test]
    fn chat_strings_round_trip() {
        for row in &EFFORT_TABLE {
            assert_eq!(Effort::from_chat(row.chat), Some(row.effort));
        }
        assert_eq!(Effort::from_chat("extreme"), None);
    }
}
