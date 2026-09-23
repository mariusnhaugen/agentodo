use crate::model::Priority;

/// The result of parsing a one-line quick-add string such as
/// `ship release !high +work #urgent`.
///
/// Fields not mentioned in the input are `None`/empty so callers can tell
/// "not specified" apart from "set to the default".
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct QuickAdd {
    pub title: String,
    pub priority: Option<Priority>,
    pub project: Option<String>,
    pub tags: Vec<String>,
}

/// Parses inline tokens out of a todo title.
///
/// Whitespace-separated words are recognised as:
/// - `!high`/`!h`, `!medium`/`!m`, `!low`/`!l` — priority (last one wins)
/// - `+name` — project (last one wins)
/// - `#name` — tag (deduplicated, in order)
///
/// Anything else, including unknown `!words` and a bare `+` or `#`, stays in
/// the title. Tokens only count at the start of a word, so `c#` is plain text.
pub fn parse_quick_add(input: &str) -> QuickAdd {
    let mut out = QuickAdd::default();
    let mut words = Vec::new();

    for word in input.split_whitespace() {
        if let Some(p) = word.strip_prefix('!').and_then(priority_token) {
            out.priority = Some(p);
        } else if let Some(project) = word.strip_prefix('+').filter(|s| !s.is_empty()) {
            out.project = Some(project.to_owned());
        } else if let Some(tag) = word.strip_prefix('#').filter(|s| !s.is_empty()) {
            if !out.tags.iter().any(|t| t == tag) {
                out.tags.push(tag.to_owned());
            }
        } else {
            words.push(word);
        }
    }

    out.title = words.join(" ");
    out
}

fn priority_token(s: &str) -> Option<Priority> {
    match s.to_ascii_lowercase().as_str() {
        "high" | "h" => Some(Priority::High),
        "medium" | "med" | "m" => Some(Priority::Medium),
        "low" | "l" => Some(Priority::Low),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_all_token_kinds() {
        let q = parse_quick_add("ship release !high +work #urgent #release");
        assert_eq!(q.title, "ship release");
        assert_eq!(q.priority, Some(Priority::High));
        assert_eq!(q.project.as_deref(), Some("work"));
        assert_eq!(q.tags, vec!["urgent", "release"]);
    }

    #[test]
    fn plain_title_leaves_fields_unset() {
        let q = parse_quick_add("  buy   milk ");
        assert_eq!(
            q,
            QuickAdd {
                title: "buy milk".into(),
                ..Default::default()
            }
        );
    }

    #[test]
    fn short_priority_aliases_and_last_wins() {
        assert_eq!(parse_quick_add("x !l").priority, Some(Priority::Low));
        assert_eq!(parse_quick_add("x !h !m").priority, Some(Priority::Medium));
        assert_eq!(parse_quick_add("x +a +b").project.as_deref(), Some("b"));
    }

    #[test]
    fn non_tokens_stay_in_title() {
        let q = parse_quick_add("learn c# + rust !now # done");
        assert_eq!(q.title, "learn c# + rust !now # done");
        assert_eq!(q.priority, None);
        assert_eq!(q.project, None);
        assert!(q.tags.is_empty());
    }

    #[test]
    fn duplicate_tags_are_collapsed() {
        assert_eq!(parse_quick_add("x #a #b #a").tags, vec!["a", "b"]);
    }
}
