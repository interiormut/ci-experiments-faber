//! HTML to Markdown, and the size bound on the result.
//!
//! Markdown rather than plain text because the structure *is* information: an
//! agent that cannot tell a heading from a paragraph, a code block from prose,
//! or a table cell from its neighbour has been handed a worse document than
//! the one that arrived. `htmd` handles the conversion; what this module adds
//! is tidying and the truncation rule.
//!
//! **Truncation is always flagged.** This follows the repo's own tool-surface
//! rule that a capped result the model reads as complete is worse than no
//! result, because it ends the search. So a cut is recorded in the returned
//! [`Truncation`] *and* written into the Markdown itself — a caller that only
//! ever looks at the text still sees that there was more.

use htmd::HtmlToMarkdown;
use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};

/// What was left out, when anything was.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Truncation {
    /// Bytes of Markdown kept.
    pub kept_bytes: usize,
    /// Bytes the conversion actually produced.
    pub total_bytes: usize,
}

impl Truncation {
    /// The line appended to truncated Markdown.
    ///
    /// Phrased for a reader who has just run out of document mid-sentence and
    /// needs to know whether that is the end of the page or the end of the
    /// budget.
    pub fn notice(&self) -> String {
        format!(
            "\n\n---\n\n*[truncated: {} of {} bytes of Markdown shown; \
             re-read this page with a larger output budget, or fetch a more specific URL]*",
            self.kept_bytes, self.total_bytes
        )
    }
}

pub fn convert(html: &str) -> Result<String> {
    let options = htmd::options::Options {
        // `-` and one space: the CommonMark spelling most models have seen
        // most of, and two fewer bytes per list item than the default
        // `*` with three spaces.
        bullet_list_marker: htmd::options::BulletListMarker::Dash,
        ul_bullet_spacing: 1,
        ol_number_spacing: 1,
        ..Default::default()
    };
    let converter = HtmlToMarkdown::builder()
        .options(options)
        // Belt and braces: `clean::strip` has already removed these from the
        // tree, but article extraction can reintroduce wrappers and the cost
        // of saying so twice is nothing.
        .skip_tags(vec!["script", "style", "noscript", "svg", "iframe"])
        .build();
    converter
        .convert(html)
        .map(tidy)
        .map_err(|error| Error::Render(error.to_string()))
}

/// Collapses the two kinds of padding that converted HTML is full of.
///
/// **Blank-line runs.** Nested divs and empty wrappers turn into stretches of
/// blank lines, which cost tokens and read as section breaks that were never
/// in the page.
///
/// **Table alignment.** Markdown writers pad every cell out to the width of
/// the widest one in the column, which is a courtesy to a human reading the
/// source and pure waste to everything else: on a Wikipedia infobox it
/// averaged 1.5 KB per row, nearly all of it spaces. Alignment padding has no
/// effect on how a table renders or parses, so it goes.
///
/// Fenced code blocks are left exactly as they are — whitespace is content
/// there, and a line inside one that happens to start with `|` is not a table.
fn tidy(markdown: String) -> String {
    let mut out = String::with_capacity(markdown.len());
    let mut blank_run = 0usize;
    let mut in_fence = false;

    for line in markdown.lines() {
        let trimmed = line.trim_end();
        let starts_fence = {
            let start = trimmed.trim_start();
            start.starts_with("```") || start.starts_with("~~~")
        };
        if starts_fence {
            in_fence = !in_fence;
        }

        if trimmed.is_empty() && !in_fence {
            blank_run += 1;
            // One blank line separates blocks; more is noise.
            if blank_run > 1 {
                continue;
            }
        } else {
            blank_run = 0;
        }

        if !in_fence && !starts_fence && is_table_row(trimmed) {
            out.push_str(&unpad_row(trimmed));
        } else {
            out.push_str(trimmed);
        }
        out.push('\n');
    }
    out.trim().to_owned()
}

fn is_table_row(line: &str) -> bool {
    line.trim_start().starts_with('|')
}

/// Squeezes a table row's alignment padding without touching its cells.
fn unpad_row(line: &str) -> String {
    let indent_len = line.len() - line.trim_start().len();
    let (indent, body) = line.split_at(indent_len);

    // A delimiter row is only dashes, colons, pipes and spaces; its runs of
    // dashes are padding too, and three is all Markdown needs.
    let delimiter = body
        .chars()
        .all(|c| matches!(c, '|' | '-' | ':' | ' ' | '\t'));

    let mut out = String::with_capacity(body.len());
    out.push_str(indent);
    let mut spaces = 0usize;
    let mut dashes = 0usize;
    for character in body.chars() {
        match character {
            ' ' | '\t' => {
                spaces += 1;
                dashes = 0;
                if spaces == 1 {
                    out.push(' ');
                }
            }
            '-' if delimiter => {
                spaces = 0;
                dashes += 1;
                if dashes <= 3 {
                    out.push('-');
                }
            }
            other => {
                spaces = 0;
                dashes = 0;
                out.push(other);
            }
        }
    }
    out.trim_end().to_owned()
}

/// Caps the Markdown, preferring to cut at a paragraph or line boundary.
///
/// Returns the text and, when a cut happened, what it cost. A limit of zero
/// means no limit.
pub fn truncate(markdown: String, limit: usize) -> (String, Option<Truncation>) {
    if limit == 0 || markdown.len() <= limit {
        return (markdown, None);
    }

    let total_bytes = markdown.len();
    // Leave room for the notice so the result still honours the caller's
    // budget once the flag is appended.
    let reserve = 200;
    let target = limit.saturating_sub(reserve).max(limit / 2);

    let mut end = target.min(markdown.len());
    while end > 0 && !markdown.is_char_boundary(end) {
        end -= 1;
    }
    // Back up to a line break if one is close, so the text does not stop
    // mid-sentence when it does not have to.
    if let Some(break_at) = markdown[..end]
        .rfind("\n\n")
        .or_else(|| markdown[..end].rfind('\n'))
        && break_at > end.saturating_sub(2000)
    {
        end = break_at;
    }

    let truncation = Truncation {
        kept_bytes: end,
        total_bytes,
    };
    let mut kept = markdown[..end].trim_end().to_owned();
    kept.push_str(&truncation.notice());
    (kept, Some(truncation))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn structure_survives_the_conversion() {
        let markdown = convert(
            "<h1>Title</h1><p>Some <strong>bold</strong> text and a \
             <a href=\"https://example.org\">link</a>.</p>\
             <pre><code>fn main() {}</code></pre><ul><li>one</li><li>two</li></ul>",
        )
        .unwrap();
        assert!(markdown.contains("# Title"));
        assert!(markdown.contains("**bold**"));
        assert!(markdown.contains("[link](https://example.org)"));
        assert!(markdown.contains("fn main() {}"));
        assert!(markdown.contains("- one"));
    }

    #[test]
    fn tables_survive_because_the_cells_are_the_content() {
        let markdown = convert(
            "<table><thead><tr><th>Type</th><th>Bits</th></tr></thead>\
             <tbody><tr><td>u8</td><td>8</td></tr></tbody></table>",
        )
        .unwrap();
        assert!(markdown.contains("Type"), "{markdown}");
        assert!(markdown.contains("u8"));
        assert!(markdown.contains('|'), "expected a pipe table: {markdown}");
    }

    /// Measured on a Wikipedia infobox, alignment padding was averaging
    /// 1.5 KB per row against about 280 bytes of content.
    #[test]
    fn table_alignment_padding_is_squeezed_out() {
        let padded = "| Name        | Value                |\n\
                      | ----------- | -------------------- |\n\
                      | rows        | 27                   |";
        let tidied = tidy(padded.to_owned());
        assert_eq!(tidied, "| Name | Value |\n| --- | --- |\n| rows | 27 |");
        assert!(tidied.len() < padded.len() / 2);
    }

    #[test]
    fn alignment_markers_and_cell_text_survive_the_squeeze() {
        let tidied = tidy(
            "| a    |  b   |\n| :--- | ---: |\n| some text here | [x](https://e.org) |".to_owned(),
        );
        assert!(tidied.contains("| :--- | ---: |"), "{tidied}");
        assert!(tidied.contains("some text here"));
        assert!(tidied.contains("[x](https://e.org)"));
    }

    /// Whitespace is content inside a fence, and a line starting with `|` in
    /// there is code, not a table.
    #[test]
    fn code_blocks_are_left_exactly_as_they_are() {
        let source =
            "```rust\nmatch x {\n    Some(v) | None => {}\n}\n\n\n\nlet y = 1;\n```\n\ntext";
        let tidied = tidy(source.to_owned());
        assert!(tidied.contains("    Some(v) | None => {}"), "{tidied}");
        assert!(
            tidied.contains("\n\n\n\nlet y = 1;"),
            "blank lines inside a fence are content: {tidied:?}"
        );
    }

    #[test]
    fn runs_of_blank_lines_collapse() {
        let markdown = tidy("a\n\n\n\n\nb\n   \n  \nc\n\n\n".to_owned());
        assert_eq!(markdown, "a\n\nb\n\nc");
    }

    #[test]
    fn a_short_document_is_untouched() {
        let (text, truncation) = truncate("short".to_owned(), 1000);
        assert_eq!(text, "short");
        assert!(truncation.is_none());
    }

    #[test]
    fn truncation_is_flagged_in_the_text_as_well_as_the_struct() {
        let long = "paragraph one\n\n".repeat(1000);
        let original = long.len();
        let (text, truncation) = truncate(long, 4096);

        let truncation = truncation.expect("should have been cut");
        assert_eq!(truncation.total_bytes, original);
        assert!(truncation.kept_bytes < original);
        assert!(
            text.contains("truncated"),
            "the text must say so on its own"
        );
        assert!(
            text.len() <= 4096,
            "the notice must fit inside the caller's budget, got {}",
            text.len()
        );
    }

    #[test]
    fn truncation_never_splits_a_character() {
        let long = "한국어 문장입니다. ".repeat(500);
        let (text, truncation) = truncate(long, 1024);
        assert!(truncation.is_some());
        // Round-tripping proves every byte kept is a whole character.
        assert!(text.contains("한국어"));
    }

    #[test]
    fn a_zero_limit_means_no_limit() {
        let long = "x".repeat(100_000);
        let (text, truncation) = truncate(long, 0);
        assert_eq!(text.len(), 100_000);
        assert!(truncation.is_none());
    }
}
