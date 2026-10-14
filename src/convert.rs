use serde_json::Value;
use std::process::Stdio;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;
use tokio::{io::AsyncWriteExt, process::Command};
use tracing::{debug, info, instrument, warn};

#[derive(Debug, thiserror::Error)]
pub enum ConvertError {
    #[error("file has no extension, cannot detect type")]
    NoExtension,
    #[error("unsupported file type: .{0}")]
    Unsupported(String),
    #[error("html conversion failed: {0}")]
    Html(std::io::Error),
    #[error("pdf has no extractable text (scanned images need OCR)")]
    NoText,
    #[error("{0} timed out")]
    Timeout(&'static str),
    #[error("{0} failed: {1}")]
    Tool(&'static str, String),
    #[error("converter output is not valid UTF-8")]
    Utf8(#[from] std::string::FromUtf8Error),
    #[error("failed to run converter: {0}")]
    Io(#[from] std::io::Error),
}

impl ConvertError {
    /// True when the failure is ours (missing binary, broken pipe), not the uploaded file's.
    pub fn is_internal(&self) -> bool {
        matches!(self, ConvertError::Io(_))
    }

    pub fn is_unsupported(&self) -> bool {
        matches!(self, ConvertError::NoExtension | ConvertError::Unsupported(_))
    }
}

pub struct Converted {
    pub markdown: String,
    /// YAML frontmatter of markdown uploads, as JSON.
    pub metadata: Option<Value>,
}

impl From<String> for Converted {
    fn from(markdown: String) -> Self {
        Converted { markdown, metadata: None }
    }
}

/// Page chrome that would otherwise land in every chunk. `<header>` is kept: article
/// headers often hold the page's only `<h1>`.
const HTML_SKIP: [&str; 7] = ["head", "script", "style", "noscript", "template", "nav", "footer"];

/// Converts an uploaded file to markdown, picking the converter by file extension.
#[instrument(skip(data), fields(bytes = data.len()))]
pub async fn to_markdown(filename: &str, data: &[u8]) -> Result<Converted, ConvertError> {
    let ext = std::path::Path::new(filename)
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    debug!(ext, "picked converter");

    Ok(match ext.as_str() {
        "md" | "markdown" => {
            let text = decode_text(data);
            let (metadata, body) = split_frontmatter(&text);
            Converted { markdown: body.to_string(), metadata }
        }
        "txt" => escape_plain(&decode_text(data)).into(),
        "html" | "htm" => htmd::HtmlToMarkdown::builder()
            .skip_tags(HTML_SKIP.to_vec())
            .build()
            .convert(&decode_text(data))
            .map_err(ConvertError::Html)?
            .into(),
        "pdf" => pdf(data).await?.into(),
        // Binary formats go to pandoc untouched.
        "docx" | "odt" | "epub" | "pptx" | "xlsx" | "rtf" | "ipynb" => pandoc(&ext, data).await?.into(),
        // Legacy binary Office: LibreOffice upgrades it to the OOXML twin pandoc reads.
        "doc" | "xls" | "ppt" => {
            let modern = format!("{ext}x");
            pandoc(&modern, &office(&ext, &modern, data).await?).await?.into()
        }
        // Pandoc only reads UTF-8 text.
        "rst" | "org" | "textile" | "csv" | "tsv" => pandoc(&ext, decode_text(data).as_bytes()).await?.into(),
        "tex" | "latex" => pandoc("latex", decode_text(data).as_bytes()).await?.into(),
        "" => return Err(ConvertError::NoExtension),
        other => return Err(ConvertError::Unsupported(other.to_string())),
    })
}

/// Runs each external converter once so its first real use isn't slowed by process and
/// library start-up (measured: pandoc ~0.4s, pdftohtml ~0.1s, soffice ~0.6s extra).
/// A failure only warns: the matching formats will then fail per request.
pub async fn warm_up() {
    let started = std::time::Instant::now();
    let (pandoc, pdftohtml, soffice) = tokio::join!(
        warm_pandoc(),
        run("pdftohtml", &["-v"], &[]),
        office("txt", "docx", b"warm-up"),
    );
    for (tool, err) in [
        ("pandoc", pandoc.err()),
        ("pdftohtml", pdftohtml.err()),
        ("soffice", soffice.err()),
    ] {
        if let Some(err) = err {
            warn!(tool, error = %err, "converter warm-up failed");
        }
    }
    info!(ms = started.elapsed().as_millis() as u64, "converters warmed up");
}

/// Text and zip-based readers are separate code in pandoc, so warm both: CSV, then a docx
/// round trip (the zip/XML path docx, odt, epub, pptx and xlsx share).
async fn warm_pandoc() -> Result<(), ConvertError> {
    let csv = b"a,b\n1,2\n";
    to_markdown("warm-up.csv", csv).await?;
    let path = temp_path(".docx");
    let made = run("pandoc", &["-f", "csv", "-t", "docx", "-o", &path.to_string_lossy()], csv).await;
    let docx = match made {
        Ok(_) => tokio::fs::read(&path).await.map_err(ConvertError::from),
        Err(err) => Err(err),
    };
    let _ = tokio::fs::remove_file(&path).await;
    pandoc("docx", &docx?).await.map(drop)
}

/// UTF-8, or UTF-16 with a BOM, as-is; any other bytes are read as Windows-1252.
// ponytail: no charset sniffing, so Shift-JIS, GBK etc. and HTML <meta charset> are ignored.
// Add chardetng if those show up.
fn decode_text(data: &[u8]) -> String {
    if let Some((encoding, _)) = encoding_rs::Encoding::for_bom(data) {
        return encoding.decode_with_bom_removal(data).0.into_owned();
    }
    match std::str::from_utf8(data) {
        Ok(text) => text.to_string(),
        Err(_) => encoding_rs::WINDOWS_1252.decode_without_bom_handling(data).0.into_owned(),
    }
}

/// Plain text as markdown: escapes line starts markdown would read as headings, lists,
/// quotes, tables or HTML, so every line stays literal text.
fn escape_plain(text: &str) -> String {
    text.lines()
        .map(|line| {
            let indent = line.len() - line.trim_start_matches(' ').len();
            let (lead, rest) = line.split_at(indent);
            let digits = rest.bytes().take_while(u8::is_ascii_digit).count();
            if indent >= 4 {
                line.to_string() // indented code block: shown verbatim already
            } else if rest.starts_with(['#', '>', '-', '+', '*', '=', '_', '`', '~', '<', '|']) {
                format!("{lead}\\{rest}")
            } else if digits > 0 && rest[digits..].starts_with(['.', ')']) {
                format!("{lead}{}\\{}", &rest[..digits], &rest[digits..])
            } else {
                line.to_string()
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Splits a leading `---` YAML block off markdown. YAML that does not parse is kept as a string.
fn split_frontmatter(md: &str) -> (Option<Value>, &str) {
    let Some(rest) = md.strip_prefix("---\n").or_else(|| md.strip_prefix("---\r\n")) else {
        return (None, md);
    };
    let mut offset = 0;
    for line in rest.split_inclusive('\n') {
        if matches!(line.trim_end(), "---" | "...") {
            let yaml = &rest[..offset];
            let meta = serde_yaml_ng::from_str(yaml).unwrap_or_else(|_| Value::String(yaml.to_string()));
            return (Some(meta), &rest[offset + line.len()..]);
        }
        offset += line.len();
    }
    (None, md) // no closing fence: not frontmatter
}

/// A fresh path in the temp dir, unique across processes and concurrent requests.
fn temp_path(suffix: &str) -> std::path::PathBuf {
    static SEQ: AtomicU64 = AtomicU64::new(0);
    let seq = SEQ.fetch_add(1, Ordering::Relaxed);
    std::env::temp_dir().join(format!("olympia-{}-{seq}{suffix}", std::process::id()))
}

async fn pdf(data: &[u8]) -> Result<String, ConvertError> {
    // pdftohtml cannot read stdin, so the upload goes through a temp file.
    let path = temp_path(".pdf");
    tokio::fs::write(&path, data).await?;
    let file = path.to_string_lossy();
    let xml = run("pdftohtml", &["-xml", "-i", "-stdout", &file], &[]).await;
    let _ = tokio::fs::remove_file(&path).await;

    let markdown = crate::pdf::layout_to_markdown(&xml?);
    if markdown.is_empty() {
        return Err(ConvertError::NoText);
    }
    Ok(markdown)
}

/// Converts a .doc/.xls/.ppt to `target` (docx/xlsx/pptx) with headless LibreOffice.
async fn office(ext: &str, target: &str, data: &[u8]) -> Result<Vec<u8>, ConvertError> {
    let dir = temp_path("");
    tokio::fs::create_dir(&dir).await?;
    let input = dir.join(format!("in.{ext}"));
    let result = convert_office(&dir, &input, target, data).await;
    let _ = tokio::fs::remove_dir_all(&dir).await;
    result
}

async fn convert_office(dir: &std::path::Path, input: &std::path::Path, target: &str, data: &[u8]) -> Result<Vec<u8>, ConvertError> {
    tokio::fs::write(input, data).await?;
    // Own profile per run: soffice locks its profile, so concurrent runs sharing one would fail.
    let profile = format!("-env:UserInstallation=file://{}", dir.join("profile").display());
    let (outdir, file) = (dir.to_string_lossy(), input.to_string_lossy());
    let args = [profile.as_str(), "--headless", "--norestore", "--convert-to", target, "--outdir", &outdir, &file];
    run("soffice", &args, &[]).await?;
    // soffice exits 0 even when it cannot load the file; it just writes nothing.
    tokio::fs::read(input.with_extension(target))
        .await
        .map_err(|_| ConvertError::Tool("soffice", "could not read the file".into()))
}

async fn pandoc(format: &str, data: &[u8]) -> Result<String, ConvertError> {
    // --sandbox stops formats like LaTeX from reading local files via \input.
    run("pandoc", &["--sandbox", "--wrap=none", "-f", format, "-t", "gfm"], data).await
}

/// Wall-clock cap per converter run, so one hostile or huge upload can't hold a worker forever.
const TIMEOUT: Duration = Duration::from_secs(120);

/// Runs a converter, feeding `input` on stdin and returning stdout.
async fn run(program: &'static str, args: &[&str], input: &[u8]) -> Result<String, ConvertError> {
    run_for(program, args, input, TIMEOUT).await
}

async fn run_for(program: &'static str, args: &[&str], input: &[u8], timeout: Duration) -> Result<String, ConvertError> {
    let mut child = Command::new(program)
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        // Own process group, so a timeout also reaches helpers it forks (soffice runs soffice.bin).
        .process_group(0)
        .kill_on_drop(true)
        .spawn()?;
    let pid = child.id();

    let mut stdin = child.stdin.take().expect("stdin is piped");
    let input = input.to_vec();
    let writer = tokio::spawn(async move { stdin.write_all(&input).await });
    let Ok(out) = tokio::time::timeout(timeout, child.wait_with_output()).await else {
        if let Some(pid) = pid {
            // SAFETY: plain syscall; the group id is the child's pid, set by process_group(0).
            unsafe { libc::killpg(pid as libc::pid_t, libc::SIGKILL) };
        }
        return Err(ConvertError::Timeout(program));
    };
    let out = out?;

    if !out.status.success() {
        return Err(ConvertError::Tool(program, String::from_utf8_lossy(&out.stderr).trim().to_string()));
    }
    writer.await.map_err(std::io::Error::other)??;
    Ok(String::from_utf8(out.stdout)?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn text_inputs_survive_conversion() {
        // Plain text: nothing may turn into a heading, list or quote.
        let txt = "Report\n======\n# not a heading\n1. not a list\n  > not a quote\nplain";
        let md = to_markdown("a.txt", txt.as_bytes()).await.unwrap().markdown;
        assert_eq!(md, "Report\n\\======\n\\# not a heading\n1\\. not a list\n  \\> not a quote\nplain");
        let chunks = crate::chunk::chunk(&md);
        assert_eq!(chunks.len(), 1);
        assert!(chunks[0].heading_path.is_empty() && chunks[0].content.contains("not a heading"));

        // Frontmatter becomes metadata; bad YAML is kept raw; no closing fence means no frontmatter.
        let c = to_markdown("a.md", b"---\ntitle: Guide\ntags: [a]\n---\n# Intro\n").await.unwrap();
        assert_eq!(c.metadata, Some(serde_json::json!({"title": "Guide", "tags": ["a"]})));
        assert_eq!(c.markdown, "# Intro\n");
        assert_eq!(split_frontmatter("---\n: [\n---\nx").0, Some(Value::String(": [\n".into())));
        assert_eq!(split_frontmatter("---\nno end").0, None);

        // Encodings: Windows-1252 and BOM-marked UTF-16 decode; UTF-8 is untouched.
        assert_eq!(decode_text(b"\xe9t\xe9 caf\xe9"), "été café");
        assert_eq!(decode_text(b"\xff\xfeh\x00\xe9\x00"), "hé");
        assert_eq!(decode_text("déjà".as_bytes()), "déjà");

        // HTML page chrome is dropped, content and tables kept.
        let html = "<html><head><title>T</title><style>.x{}</style></head><body><nav>Home</nav>\
                    <script>alert(1)</script><h1>T</h1><p>Para</p><footer>(c)</footer></body></html>";
        let md = to_markdown("a.html", html.as_bytes()).await.unwrap().markdown;
        assert_eq!(md.trim(), "# T\n\nPara");
    }

    #[tokio::test]
    async fn slow_converter_times_out() {
        let started = std::time::Instant::now();
        let err = run_for("sleep", &["5"], b"", Duration::from_millis(200)).await.unwrap_err();
        assert!(matches!(err, ConvertError::Timeout("sleep")), "{err:?}");
        assert!(started.elapsed() < Duration::from_secs(2));

        // Processes the converter forked die with it.
        let pid_file = temp_path(".pid");
        let script = format!("sleep 30 & echo $! > {}; wait", pid_file.display());
        let err = run_for("sh", &["-c", &script], b"", Duration::from_millis(300)).await.unwrap_err();
        assert!(matches!(err, ConvertError::Timeout("sh")), "{err:?}");
        let grandchild: libc::pid_t = std::fs::read_to_string(&pid_file).unwrap().trim().parse().unwrap();
        let _ = std::fs::remove_file(&pid_file);
        tokio::time::sleep(Duration::from_millis(100)).await;
        // SAFETY: signal 0 only checks whether the process exists.
        assert_ne!(unsafe { libc::kill(grandchild, 0) }, 0, "forked process survived the timeout");
    }
}
