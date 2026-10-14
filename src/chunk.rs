use pulldown_cmark::{Event, HeadingLevel, Options, Parser, Tag, TagEnd};
use serde::Serialize;
use std::borrow::Cow;
use std::sync::LazyLock;
use tiktoken_rs::cl100k_base_singleton;

/// Chunk budget in cl100k_base tokens, the tokenizer of OpenAI's text-embedding-3 models.
/// Sections over it split at block boundaries; single blocks over it split at sentences.
/// Set with CHUNK_MAX_TOKENS (default 512).
pub static MAX_TOKENS: LazyLock<usize> = LazyLock::new(|| match std::env::var("CHUNK_MAX_TOKENS") {
    Ok(v) => v.trim().parse().ok().filter(|&n| n > 0)
        .unwrap_or_else(|| panic!("CHUNK_MAX_TOKENS must be a positive integer, got {v:?}")),
    Err(_) => 512,
});

#[derive(Serialize, Debug)]
pub struct Chunk {
    /// Headings from the top level down to this chunk's section, e.g. ["Guide", "Setup"].
    pub heading_path: Vec<String>,
    /// Markdown body of the section (without its heading line).
    pub content: String,
}

/// Splits markdown into one chunk per heading section, keeping top-level blocks
/// (paragraphs, lists, tables, code) intact unless one alone exceeds MAX_TOKENS.
pub fn chunk(md: &str) -> Vec<Chunk> {
    let opts = Options::ENABLE_TABLES | Options::ENABLE_STRIKETHROUGH | Options::ENABLE_TASKLISTS | Options::ENABLE_FOOTNOTES;
    let mut chunks = Vec::new();
    let mut path: Vec<(HeadingLevel, String)> = Vec::new();
    let mut buf = String::new();
    let mut buf_tokens = 0;
    let mut heading: Option<(HeadingLevel, String)> = None;
    let mut depth = 0;

    for (event, range) in Parser::new_ext(md, opts).into_offset_iter() {
        match event {
            Event::Start(tag) => {
                if depth == 0 {
                    if let Tag::Heading { level, .. } = tag {
                        heading = Some((level, String::new()));
                    } else {
                        let block = md[range].trim_end();
                        let pieces = if matches!(tag, Tag::Table(_)) { fit_table(block) } else { fit(block) };
                        for piece in pieces {
                            let n = tokens(&piece);
                            if buf_tokens > 0 && buf_tokens + n > *MAX_TOKENS {
                                flush(&mut chunks, &path, &mut buf, &mut buf_tokens);
                            }
                            buf.push_str(&piece);
                            buf.push_str("\n\n");
                            buf_tokens += n;
                        }
                    }
                }
                depth += 1;
            }
            Event::End(TagEnd::Heading(_)) if depth == 1 => {
                depth -= 1;
                let (level, text) = heading.take().expect("heading started");
                flush(&mut chunks, &path, &mut buf, &mut buf_tokens);
                while path.last().is_some_and(|(l, _)| *l >= level) {
                    path.pop();
                }
                path.push((level, text.trim().to_string()));
            }
            Event::End(_) => depth -= 1,
            Event::Text(t) | Event::Code(t) => {
                if let Some((_, text)) = heading.as_mut() {
                    text.push_str(&t);
                }
            }
            _ => {}
        }
    }
    flush(&mut chunks, &path, &mut buf, &mut buf_tokens);
    chunks
}

fn flush(chunks: &mut Vec<Chunk>, path: &[(HeadingLevel, String)], buf: &mut String, buf_tokens: &mut usize) {
    let content = buf.trim();
    if !content.is_empty() {
        chunks.push(Chunk {
            heading_path: path.iter().map(|(_, h)| h.clone()).collect(),
            content: content.to_string(),
        });
    }
    buf.clear();
    *buf_tokens = 0;
}

fn tokens(text: &str) -> usize {
    cl100k_base_singleton().encode_ordinary(text).len()
}

/// Splits a block over MAX_TOKENS at sentence ends, then at spaces for over-long sentences.
// ponytail: a huge table or code block split this way loses its markdown shape; fine for prose.
// A single "word" over MAX_TOKENS stays oversized.
fn fit(block: &str) -> Vec<Cow<'_, str>> {
    if tokens(block) <= *MAX_TOKENS {
        return vec![block.into()];
    }
    pack(block, cuts(block, true))
        .into_iter()
        .flat_map(|s| if tokens(s) <= *MAX_TOKENS { vec![s] } else { pack(s, cuts(s, false)) })
        .map(Cow::Borrowed)
        .collect()
}

/// Splits a table over MAX_TOKENS by rows, repeating its header and delimiter rows in
/// every piece so each one is a valid table on its own.
// ponytail: a single row over the budget stays oversized.
fn fit_table(table: &str) -> Vec<Cow<'_, str>> {
    let rows: Vec<&str> = table.lines().collect();
    if rows.len() < 3 || tokens(table) <= *MAX_TOKENS {
        return vec![table.into()];
    }
    let head = format!("{}\n{}", rows[0], rows[1]);
    let head_tokens = tokens(&head);
    let mut pieces = Vec::new();
    let (mut piece, mut count) = (head.clone(), head_tokens);
    for row in &rows[2..] {
        let n = tokens(&format!("\n{row}"));
        if count > head_tokens && count + n > *MAX_TOKENS {
            pieces.push(std::mem::replace(&mut piece, head.clone()).into());
            count = head_tokens;
        }
        piece.push('\n');
        piece.push_str(row);
        count += n;
    }
    pieces.push(piece.into());
    pieces
}

/// Positions of whitespace that ends a word (or, with `sentence`, a sentence).
// ponytail: sentence end = . ! ? … before whitespace; abbreviations like "M. Pérez" cut early.
fn cuts(text: &str, sentence: bool) -> impl Iterator<Item = usize> + '_ {
    let mut prev = ' ';
    text.char_indices().filter_map(move |(i, c)| {
        let cut = c.is_whitespace() && !prev.is_whitespace() && (!sentence || matches!(prev, '.' | '!' | '?' | '…'));
        prev = c;
        cut.then_some(i)
    })
}

/// Greedily groups the spans between `cuts` into pieces of at most MAX_TOKENS.
/// Spans start with their leading space, so per-span counts add up to the piece's count.
fn pack(text: &str, cuts: impl Iterator<Item = usize>) -> Vec<&str> {
    let mut out = Vec::new();
    let (mut start, mut prev, mut count) = (0, 0, 0);
    for cut in cuts.chain([text.len()]) {
        let n = tokens(&text[prev..cut]);
        if count > 0 && count + n > *MAX_TOKENS {
            out.push(&text[start..prev]);
            start = prev;
            count = 0;
        }
        count += n;
        prev = cut;
    }
    out.push(&text[start..]);
    out.into_iter().map(str::trim).filter(|s| !s.is_empty()).collect()
}

#[cfg(test)]
mod tests {
    use super::{MAX_TOKENS, chunk, tokens};

    #[test]
    fn chunks_by_heading_and_size() {
        let md = "intro\n\n# A\n\npara a\n\n## B\n\npara b\n\n```\n# not a heading\n```\n\n# C\n\npara c\n\nSetext\n------\n\npara d\n";
        let c = chunk(md);
        let paths: Vec<_> = c.iter().map(|c| c.heading_path.join(" > ")).collect();
        assert_eq!(paths, ["", "A", "A > B", "C", "C > Setext"]);
        assert!(c[2].content.contains("# not a heading"));

        // 400-token paragraphs: two don't fit in 512, so one chunk each.
        let para = vec!["word"; 400].join(" ");
        assert_eq!(tokens(&para), 400);
        let big = format!("# Big\n\n{para}\n\n{para}\n\n{para}\n");
        let c = chunk(&big);
        assert_eq!(c.len(), 3);
        assert!(c.iter().all(|c| c.heading_path == ["Big"] && c.content == para));

        // One ~900-token paragraph splits at sentence ends.
        let sentence = format!("{}.", vec!["word"; 300].join(" "));
        let c = chunk(&format!("# Long\n\n{sentence} {sentence} {sentence}\n"));
        assert_eq!(c.len(), 3);
        assert!(c.iter().all(|c| c.content == sentence && tokens(&c.content) <= *MAX_TOKENS));

        // A big table splits by rows, each piece keeping the header.
        let rows: Vec<String> = (0..300).map(|i| format!("| {i} | value {i} |")).collect();
        let table = format!("| id | value |\n| - | - |\n{}", rows.join("\n"));
        let c = chunk(&format!("# T\n\n{table}\n"));
        assert!(c.len() > 1);
        for piece in &c {
            assert!(piece.content.starts_with("| id | value |\n| - | - |\n| "));
            assert!(tokens(&piece.content) <= *MAX_TOKENS, "{}", tokens(&piece.content));
        }
        let body: Vec<&str> = c.iter().flat_map(|p| p.content.lines().skip(2)).collect();
        assert_eq!(body, rows);

        // One 1000-token sentence falls back to word cuts.
        let c = chunk(&vec!["word"; 1000].join(" "));
        assert_eq!(c.len(), 2);
        assert!(c.iter().all(|c| tokens(&c.content) <= *MAX_TOKENS));
    }
}
