//! OCR for images and scanned PDFs with the Tesseract CLI, on the CPU.

use futures_util::{StreamExt, TryStreamExt, stream};

use crate::convert::{ConvertError, escape_plain, run};

/// Render resolution for scanned PDF pages; Tesseract is tuned for ~300 dpi.
const DPI: &str = "300";
/// Longer scans are refused rather than tying up the CPU for one request.
pub const MAX_PAGES: usize = 100;
// shortcut: English only; add a language setting (and its tessdata package) when needed.
const LANG: &str = "eng";

/// Text of one image upload, as literal markdown.
pub async fn image(data: &[u8]) -> Result<String, ConvertError> {
    Ok(escape_plain(run("tesseract", &["stdin", "stdout", "-l", LANG], data).await?.trim()))
}

/// Text of a PDF with no text layer: every page rendered to PNG and read by Tesseract.
pub async fn pdf(path: &std::path::Path) -> Result<String, ConvertError> {
    let dir = path.with_extension("pages");
    tokio::fs::create_dir(&dir).await?;
    let result = pdf_pages(path, &dir).await;
    let _ = tokio::fs::remove_dir_all(&dir).await;
    result
}

async fn pdf_pages(path: &std::path::Path, dir: &std::path::Path) -> Result<String, ConvertError> {
    let last = (MAX_PAGES + 1).to_string();
    let (file, prefix) = (path.to_string_lossy(), dir.join("p"));
    run("pdftoppm", &["-r", DPI, "-gray", "-png", "-l", &last, &file, &prefix.to_string_lossy()], &[]).await?;
    // pdftoppm zero-pads page numbers to equal width, so name order is page order.
    let mut pages = Vec::new();
    let mut entries = tokio::fs::read_dir(dir).await?;
    while let Some(entry) = entries.next_entry().await? {
        pages.push(entry.path());
    }
    pages.sort();
    if pages.len() > MAX_PAGES {
        return Err(ConvertError::TooManyPages(MAX_PAGES));
    }
    // One Tesseract per core; OMP_THREAD_LIMIT=1 (Dockerfile) stops each from also threading.
    let cores = std::thread::available_parallelism().map_or(1, |n| n.get());
    let texts: Vec<String> = stream::iter(pages)
        .map(|p| async move { run("tesseract", &[&p.to_string_lossy(), "stdout", "-l", LANG], &[]).await })
        .buffered(cores)
        .try_collect()
        .await?;
    let texts: Vec<String> = texts.iter().map(|t| escape_plain(t.trim())).filter(|t| !t.is_empty()).collect();
    Ok(texts.join("\n\n"))
}

#[cfg(test)]
mod tests {
    use crate::convert::to_markdown;

    #[tokio::test]
    async fn reads_images_and_scanned_pdfs() {
        let sample = |name: &str| std::fs::read(format!("{}/samples/{name}", env!("CARGO_MANIFEST_DIR"))).unwrap();
        let png = to_markdown("a.png", &sample("sample.png")).await.unwrap().markdown;
        assert!(png.contains("Lorem Ipsum Dolor"), "{png}");
        // Image-only PDF: no text layer, so every page goes through OCR, in page order.
        let pdf = to_markdown("a.pdf", &sample("sample-scanned.pdf")).await.unwrap().markdown;
        assert!(pdf.starts_with("Lorem Ipsum") && pdf.len() > 3 * png.len() / 2, "{pdf}");
        let err = crate::ocr::image(b"not an image").await.unwrap_err();
        assert!(matches!(err, crate::convert::ConvertError::Tool("tesseract", _)), "{err:?}");
    }
}
