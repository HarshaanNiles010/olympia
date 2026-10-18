//! Rebuilds markdown from poppler's `pdftohtml -xml` layout. Fonts and positions decide
//! headings, paragraphs and what is page furniture; the body text itself is never rewritten.

use std::collections::{HashMap, HashSet};

struct Line {
    page: u32,
    top: i32,
    left: i32,
    height: i32,
    chars: HashMap<String, usize>,
    text: String,
}

impl Line {
    fn font(&self) -> &str {
        self.chars.iter().max_by_key(|(_, n)| **n).map(|(f, _)| f.as_str()).unwrap_or("")
    }
}

enum Block {
    Heading(String, String),
    Para(String),
}

pub fn layout_to_markdown(xml: &str) -> String {
    let (sizes, lines) = parse(xml);
    if lines.is_empty() {
        return String::new();
    }

    // Body = the font carrying the most text.
    let mut chars_per_font: HashMap<&str, usize> = HashMap::new();
    let mut lines_per_font: HashMap<&str, usize> = HashMap::new();
    let mut pages_per_text: HashMap<(&str, String), HashSet<u32>> = HashMap::new();
    for l in &lines {
        *chars_per_font.entry(l.font()).or_default() += l.text.chars().count();
        *lines_per_font.entry(l.font()).or_default() += 1;
        pages_per_text.entry((l.font(), key(&l.text))).or_default().insert(l.page);
    }
    let body = chars_per_font.iter().max_by_key(|(_, n)| **n).map(|(f, _)| *f).unwrap_or("");
    let size = |f: &str| sizes.get(f).copied().unwrap_or(0.0);
    let repeated = |l: &Line| pages_per_text[&(l.font(), key(&l.text))].len() >= 3;

    // Heading fonts: not body, at least body size, used on short lines.
    let is_heading = |f: &str| {
        f != body && size(f) >= size(body) && chars_per_font[f] / lines_per_font[f] <= 80
    };
    let distinct_texts = |f: &str| pages_per_text.keys().filter(|(font, _)| *font == f).count();
    // Running title: a heading font that only ever shows one repeated text.
    let running_title = |l: &Line| distinct_texts(l.font()) == 1 && repeated(l);
    // Headers, footers, page numbers, nav links: repeated text in a non-body, non-heading font.
    let furniture = |l: &Line| l.font() != body && !is_heading(l.font()) && repeated(l);
    let kept: Vec<&Line> = lines
        .iter()
        .filter(|l| !furniture(l) && !(is_heading(l.font()) && running_title(l)))
        .collect();

    // ponytail: heading level = order a heading font first appears, not its size (book
    // layouts often print chapter numbers bigger than part titles). Wrong if a deeper
    // heading appears before its parent.
    let mut levels: Vec<&str> = Vec::new();
    for l in &kept {
        if is_heading(l.font()) && !levels.contains(&l.font()) {
            levels.push(l.font());
        }
    }

    let body_lines: Vec<&&Line> = kept.iter().filter(|l| l.font() == body).collect();
    let margin = mode(body_lines.iter().map(|l| l.left));
    let mut gaps: Vec<i32> = body_lines
        .windows(2)
        .filter(|w| w[0].page == w[1].page)
        .map(|w| w[1].top - w[0].top)
        .filter(|&g| g > 0 && g < 3 * body_lines[0].height.max(1))
        .collect();
    gaps.sort_unstable();
    let spacing = gaps.get(gaps.len() / 2).copied().unwrap_or(i32::MAX / 2);

    let mut blocks: Vec<Block> = Vec::new();
    let mut prev: Option<(u32, i32)> = None;
    let mut last_heading: Option<(u32, i32)> = None;
    for l in kept {
        let font = l.font();
        if is_heading(font) {
            let wrapped = last_heading
                .is_some_and(|(page, top)| page == l.page && l.top - top <= 2 * l.height);
            match blocks.last_mut() {
                // Same heading font on the next line = one wrapped heading.
                Some(Block::Heading(f, t)) if f == font && wrapped => {
                    t.push(' ');
                    t.push_str(&l.text);
                }
                _ => blocks.push(Block::Heading(font.to_string(), l.text.clone())),
            }
            last_heading = Some((l.page, l.top));
            prev = None;
            continue;
        }
        // New paragraph on first-line indent or a vertical gap; a page break alone continues it.
        // ponytail: assumes indented paragraphs or spaced paragraphs, single column.
        let continues = matches!(blocks.last(), Some(Block::Para(_)))
            && prev.is_some_and(|(page, top)| {
                l.left <= margin + 8 && (l.page != page || l.top - top <= spacing * 13 / 10)
            });
        match blocks.last_mut() {
            Some(Block::Para(p)) if continues => {
                // Keep line-end hyphens: "peut-|être" must survive, so "de-|main" stays "de-main".
                if !p.ends_with('-') {
                    p.push(' ');
                }
                p.push_str(&l.text);
            }
            _ => blocks.push(Block::Para(l.text.clone())),
        }
        prev = Some((l.page, l.top));
    }

    blocks
        .iter()
        .map(|b| match b {
            Block::Heading(f, t) => {
                let level = levels.iter().position(|x| x == f).unwrap_or(0) + 1;
                format!("{} {t}", "#".repeat(level.min(6)))
            }
            Block::Para(t) if t.starts_with('#') => format!("\\{t}"),
            Block::Para(t) => t.clone(),
        })
        .collect::<Vec<_>>()
        .join("\n\n")
}

/// Reads fontspec sizes and text lines, merging fragments that share a baseline.
fn parse(xml: &str) -> (HashMap<String, f32>, Vec<Line>) {
    let mut sizes = HashMap::new();
    let mut lines: Vec<Line> = Vec::new();
    let mut page = 0;
    // Right edge of the last fragment, to tell a column gap from glyphs set side by side.
    let mut right = 0;
    for raw in xml.lines() {
        let row = raw.trim_start();
        if row.starts_with("<page ") {
            page = attr(row, "number").and_then(|v| v.parse().ok()).unwrap_or(page + 1);
        } else if row.starts_with("<fontspec ") {
            if let (Some(id), Some(s)) = (attr(row, "id"), attr(row, "size").and_then(|s| s.parse().ok())) {
                sizes.insert(id.to_string(), s);
            }
        } else if row.starts_with("<text ") {
            let num = |k| attr(row, k).and_then(|v| v.parse::<i32>().ok()).unwrap_or(0);
            let (top, left, height, width) = (num("top"), num("left"), num("height"), num("width"));
            let font = attr(row, "font").unwrap_or("").to_string();
            let inner = row.split_once('>').map_or("", |(_, rest)| rest);
            let text = decode(&strip_tags(inner.strip_suffix("</text>").unwrap_or(inner)));
            match lines.last_mut() {
                // ponytail: only merges with the previous fragment; fine for pdftohtml's line order.
                Some(l) if l.page == page && (top - l.top).abs() <= l.height / 2 => {
                    // Table cells and side-by-side captions arrive as separate fragments.
                    if left > right + 1 {
                        l.text.push(' ');
                    }
                    l.text.push_str(&text);
                    *l.chars.entry(font).or_default() += text.chars().count();
                    l.left = l.left.min(left);
                }
                _ => lines.push(Line {
                    page,
                    top,
                    left,
                    height,
                    chars: HashMap::from([(font, text.chars().count())]),
                    text,
                }),
            }
            right = left + width;
        }
    }
    for l in &mut lines {
        l.text = l.text.split_whitespace().collect::<Vec<_>>().join(" ");
    }
    lines.retain(|l| !l.text.is_empty());
    (sizes, lines)
}

/// Text with digits stripped, so "Header 17" and "Header 18" count as the same line.
fn key(text: &str) -> String {
    text.chars().filter(|c| !c.is_ascii_digit()).collect::<String>().trim().to_string()
}

fn mode(values: impl Iterator<Item = i32>) -> i32 {
    let mut counts: HashMap<i32, usize> = HashMap::new();
    for v in values {
        *counts.entry(v).or_default() += 1;
    }
    counts.into_iter().max_by_key(|(_, n)| *n).map_or(0, |(v, _)| v)
}

fn attr<'a>(row: &'a str, name: &str) -> Option<&'a str> {
    let start = row.find(&format!(" {name}=\""))? + name.len() + 3;
    row[start..].split('"').next()
}

fn strip_tags(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut in_tag = false;
    for c in s.chars() {
        match c {
            '<' => in_tag = true,
            '>' => in_tag = false,
            _ if !in_tag => out.push(c),
            _ => {}
        }
    }
    out
}

fn decode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(i) = rest.find('&') {
        out.push_str(&rest[..i]);
        rest = &rest[i..];
        let entity = rest.find(';').map(|end| (&rest[1..end], end));
        let ch = entity.and_then(|(e, _)| match e {
            "amp" => Some('&'),
            "lt" => Some('<'),
            "gt" => Some('>'),
            "quot" => Some('"'),
            "apos" => Some('\''),
            _ => e
                .strip_prefix('#')
                .and_then(|n| match n.strip_prefix('x') {
                    Some(hex) => u32::from_str_radix(hex, 16).ok(),
                    None => n.parse().ok(),
                })
                .and_then(char::from_u32),
        });
        match (ch, entity) {
            (Some(c), Some((_, end))) => {
                out.push(c);
                rest = &rest[end + 1..];
            }
            _ => {
                out.push('&');
                rest = &rest[1..];
            }
        }
    }
    out.push_str(rest);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rebuilds_structure_from_layout() {
        let page = |n: u32, body: &str| {
            format!(
                r#"<page number="{n}" position="absolute" top="0" left="0" height="1000" width="800">
<text top="10" left="10" width="50" height="8" font="0">Doc Title {n}</text>
<text top="40" left="50" width="50" height="10" font="4">Book</text>
{body}<text top="120" left="10" width="50" height="8" font="0">Back to contents</text>
</page>"#
            )
        };
        let xml = [
            r##"<fontspec id="0" size="10" family="A" color="#000"/>
<fontspec id="1" size="12" family="A" color="#000"/>
<fontspec id="2" size="12" family="A" color="#0a0"/>
<fontspec id="3" size="30" family="A" color="#a00"/>
<fontspec id="4" size="12" family="A" color="#00a"/>"##
                .to_string(),
            page(1, r#"<text top="60" left="50" width="50" height="10" font="2">Part One</text>
<text top="80" left="50" width="20" height="30" font="3">I</text>
<text top="150" left="30" width="50" height="10" font="1">It was de-</text>
<text top="162" left="10" width="50" height="10" font="1">main, she   said</text>
<text top="161" left="60" width="5" height="11" font="0">.</text>
<text top="174" left="10" width="50" height="10" font="1">And left &amp; <b>ran</b>.</text>
<text top="186" left="30" width="50" height="10" font="1">New para here.</text>
"#),
            page(2, r#"<text top="80" left="50" width="20" height="30" font="3">II</text>
<text top="150" left="30" width="50" height="10" font="1">Chapter two text</text>
<text top="162" left="10" width="50" height="10" font="1">keeps going</text>
"#),
            page(3, r#"<text top="150" left="10" width="50" height="10" font="1">across the page.</text>
<text top="180" left="10" width="50" height="10" font="1">After a gap.</text>
<text top="192" left="10" width="30" height="10" font="1">Month</text>
<text top="192" left="60" width="20" height="10" font="1">Jan</text>
"#),
        ]
        .join("\n");

        assert_eq!(
            layout_to_markdown(&xml),
            "# Part One\n\n## I\n\nIt was de-main, she said. And left & ran.\n\nNew para here.\n\n\
             ## II\n\nChapter two text keeps going across the page.\n\nAfter a gap. Month Jan"
        );
    }
}
