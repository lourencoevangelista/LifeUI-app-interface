//! Make it yours: the few things in the web app that are about one person's life and interests.
//! Change them here and rebuild (`cd web; trunk build --release`). The news pipeline has its own settings in
//! `tools/news/sources.yaml` (keep `FOCUS_NAME` the same as `settings.focus_name` there).

/// Your main interest or project. It names the "Focus" news filter and highlights, and the Home card below.
pub const FOCUS_NAME: &str = "Focus";

/// Home → "To look into": tasks and topics from your knowledge base whose name or description contains one of
/// these words (lower case) are marked as part of your focus.
pub const FOCUS_KEYWORDS: &[&str] = &["focus", "side project"];

/// The text under the Home card's heading.
pub const FOCUS_DESCRIPTION: &str = "Topics from your notes that relate to your focus.";

/// What the app calls the machine that builds the news briefing and runs the agent.
pub const SERVER_NAME: &str = "the home server";

/// Optional: a chat page where you can ask questions about the briefing (for example an Open WebUI model that has
/// the briefing in its prompt). `None` hides the link.
pub const ASK_NEWS_URL: Option<&str> = None;
