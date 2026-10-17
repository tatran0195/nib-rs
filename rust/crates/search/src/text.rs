//! Deterministic `CommonMark` text projections. These are plain text, never
//! trusted HTML or an MDX renderer; HTTP clients must still escape result text.

use pulldown_cmark::{Event, HeadingLevel, Options, Parser, Tag, TagEnd};

const SNIPPET_CHARS: usize = 240;
const SNIPPET_CONTEXT_CHARS: usize = 80;

pub(crate) fn plain_text(markdown: &str) -> String {
    let mut text = String::new();
    for event in Parser::new_ext(
        markdown,
        Options::ENABLE_TABLES | Options::ENABLE_STRIKETHROUGH,
    ) {
        match event {
            Event::Text(value) | Event::Code(value) => text.push_str(&value),
            Event::End(
                TagEnd::Paragraph
                | TagEnd::Heading(_)
                | TagEnd::BlockQuote(_)
                | TagEnd::CodeBlock
                | TagEnd::Item
                | TagEnd::TableCell
                | TagEnd::TableRow,
            )
            | Event::SoftBreak
            | Event::HardBreak
            | Event::Rule => {
                text.push(' ');
            }
            // Never copy HTML/MDX tags, link targets, or Markdown syntax into a
            // snippet. This does not interpret arbitrary MDX expressions.
            _ => {}
        }
    }
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

pub(crate) fn headings(markdown: &str, sidebar_title: Option<&str>) -> String {
    let mut parts = Vec::new();
    if let Some(title) = sidebar_title
        .map(str::trim)
        .filter(|title| !title.is_empty())
    {
        parts.push(title.to_owned());
    }
    let mut heading = None;
    for event in Parser::new(markdown) {
        match event {
            Event::Start(Tag::Heading {
                level: HeadingLevel::H1 | HeadingLevel::H2 | HeadingLevel::H3 | HeadingLevel::H4,
                ..
            }) => {
                heading = Some(String::new());
            }
            Event::Text(value) | Event::Code(value) => {
                if let Some(text) = &mut heading {
                    text.push_str(&value);
                }
            }
            Event::End(TagEnd::Heading(_)) => {
                if let Some(text) = heading.take() {
                    parts.push(text);
                }
            }
            _ => {}
        }
    }
    parts.join(" ")
}

pub(crate) fn snippet(markdown: &str, query: &str) -> String {
    let text = plain_text(markdown);
    let characters: Vec<char> = text.chars().collect();
    // Unicode lowercasing can expand a scalar (e.g. capital dotted I). Keep
    // original scalar offsets instead of applying folded byte offsets to UTF-8.
    let mut folded = String::new();
    let mut offsets = Vec::new();
    for (index, character) in characters.iter().enumerate() {
        for lower in character.to_lowercase() {
            folded.push(lower);
            offsets.push(index);
        }
    }
    let first_term = query
        .split(|character: char| !character.is_alphanumeric())
        .find(|term| !term.is_empty())
        .unwrap_or_default()
        .to_lowercase();
    let pivot = (!first_term.is_empty())
        .then(|| folded.find(&first_term))
        .flatten()
        .and_then(|index| offsets.get(folded[..index].chars().count()).copied())
        .unwrap_or(0);
    let start = pivot.saturating_sub(SNIPPET_CONTEXT_CHARS);
    let end = (start + SNIPPET_CHARS).min(characters.len());
    let body: String = characters[start..end].iter().collect();
    format!(
        "{}{}{}",
        if start > 0 { "…" } else { "" },
        body,
        if end < characters.len() { "…" } else { "" }
    )
}

#[cfg(test)]
mod tests {
    use super::{headings, plain_text, snippet};

    #[test]
    fn markdown_projection_preserves_labels_not_tags_or_link_destinations() {
        let source = "## **Guide**\n\nRead [the manual](https://secret.example/path) and `code`.\n\n<script>hidden()</script>\n";
        assert_eq!(plain_text(source), "Guide Read the manual and code.");
        assert_eq!(snippet(source, "manual"), "Guide Read the manual and code.");
        assert_eq!(
            headings(
                "# One\n```\n## Not a heading\n```\n### `Three`\n##### Five",
                Some(" Nav ")
            ),
            "Nav One Three"
        );
    }

    #[test]
    fn snippets_are_unicode_bounded_and_centered_without_folded_offset_errors() {
        let content = format!("{} İSTANBUL 東京 {}", "語".repeat(350), "尾".repeat(350));
        let result = snippet(&content, "istanbul");
        // Simple Unicode lowercase is deliberately not language-specific
        // stemming; use a term after the expanding dotted I to prove offsets.
        let centered = snippet(&content, "東京");
        assert!(centered.contains("東京"));
        assert!(centered.starts_with('…'));
        assert!(centered.ends_with('…'));
        assert!(centered.chars().count() <= 242);
        assert!(result.chars().count() <= 242);
        assert_eq!(snippet("", "anything"), "");
        assert_eq!(snippet("東京 **guide**", "missing"), "東京 guide");
    }
}
