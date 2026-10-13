//! How full the King's context window is: the newest top-level assistant message's input
//! tokens against the window the CLI reports (or a per-model default).

/// Window of models without a reported size.
pub const DEFAULT_WINDOW: u64 = 200_000;
/// Window of the `[1m]` long-context variants.
pub const LONG_WINDOW: u64 = 1_000_000;

/// The gauge turns amber from here.
pub const WARN_PERCENT: f32 = 70.0;
/// Compact and New chat are offered from here.
pub const OFFER_PERCENT: f32 = 80.0;
/// The gauge turns red from here.
pub const CRITICAL_PERCENT: f32 = 85.0;

/// The message that compacts the conversation.
pub const COMPACT_COMMAND: &str = "/compact";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Level {
    Normal,
    Warn,
    Critical,
}

/// The context in use and the window it fills.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ContextGauge {
    pub tokens: u64,
    pub window: u64,
}

impl ContextGauge {
    /// `tokens` in the window the CLI `reported`, else the window of `model`. A count past
    /// the default window means a long-context model.
    pub fn new(tokens: u64, reported: Option<u64>, model: Option<&str>) -> Self {
        let window = reported.filter(|&w| w > 0).unwrap_or_else(|| {
            let window = default_window(model);
            if tokens > window { LONG_WINDOW } else { window }
        });
        Self { tokens, window }
    }

    pub fn percent(&self) -> f32 {
        if self.window == 0 {
            return 0.0;
        }
        self.tokens as f32 * 100.0 / self.window as f32
    }

    pub fn level(&self) -> Level {
        let p = self.percent();
        if p >= CRITICAL_PERCENT {
            Level::Critical
        } else if p >= WARN_PERCENT {
            Level::Warn
        } else {
            Level::Normal
        }
    }

    /// Whether to offer Compact and New chat.
    pub fn offers_actions(&self) -> bool {
        self.percent() >= OFFER_PERCENT
    }

    /// e.g. `Context: 142,500 / 200,000 tokens (71%)`.
    pub fn tooltip(&self) -> String {
        format!(
            "Context: {} / {} tokens ({:.0}%)",
            thousands(self.tokens),
            thousands(self.window),
            self.percent()
        )
    }
}

/// The window a model name implies: 1M for `[1m]` variants, else [`DEFAULT_WINDOW`].
pub fn default_window(model: Option<&str>) -> u64 {
    match model {
        Some(m) if m.to_ascii_lowercase().contains("[1m]") => LONG_WINDOW,
        _ => DEFAULT_WINDOW,
    }
}

fn thousands(n: u64) -> String {
    let digits = n.to_string();
    let mut out = String::new();
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn window_comes_from_the_report_the_model_or_the_count() {
        assert_eq!(ContextGauge::new(1, Some(500_000), None).window, 500_000);
        assert_eq!(
            ContextGauge::new(1, None, Some("claude-opus-5-5")).window,
            DEFAULT_WINDOW
        );
        assert_eq!(
            ContextGauge::new(1, None, Some("opus[1m]")).window,
            LONG_WINDOW
        );
        assert_eq!(ContextGauge::new(1, Some(0), None).window, DEFAULT_WINDOW);
        assert_eq!(ContextGauge::new(300_000, None, None).window, LONG_WINDOW);
    }

    #[test]
    fn thresholds() {
        let at = |tokens| ContextGauge::new(tokens, Some(200_000), None);
        assert_eq!(at(100_000).level(), Level::Normal);
        assert_eq!(at(139_999).level(), Level::Normal);
        assert_eq!(at(140_000).level(), Level::Warn);
        assert!(!at(159_999).offers_actions());
        assert!(at(160_000).offers_actions());
        assert_eq!(at(169_999).level(), Level::Warn);
        assert_eq!(at(170_000).level(), Level::Critical);
        assert_eq!(at(150_000).percent(), 75.0);
    }

    #[test]
    fn tooltip_groups_digits() {
        assert_eq!(
            ContextGauge::new(142_500, Some(1_000_000), None).tooltip(),
            "Context: 142,500 / 1,000,000 tokens (14%)"
        );
    }
}
