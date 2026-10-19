# olympia

Upload a document, get back clean markdown and heading-aware chunks ready for embedding (RAG).
Every conversion is stored in Postgres, so the same file uploaded again is answered from the database.

## Branches

| Branch | OCR |
|---|---|
| `main` | none: images and scanned (image-only) PDFs are rejected |
| `tesseract-ocr` | Tesseract on the CPU for images and scanned PDFs |

## Supported formats

| Extension | Converter |
|---|---|
| `md`, `markdown` | passed through; YAML front matter becomes `metadata` |
| `txt` | escaped plain text |
| `html`, `htm` | [htmd](https://crates.io/crates/htmd) |
| `pdf` | poppler `pdftohtml -xml`; headings, paragraphs and page furniture rebuilt from fonts and positions |
| `docx`, `odt`, `epub`, `pptx`, `xlsx`, `rtf`, `ipynb` | pandoc |
| `doc`, `xls`, `ppt` | LibreOffice upgrades to the OOXML format, then pandoc |
| `rst`, `org`, `textile`, `csv`, `tsv`, `tex`, `latex` | pandoc |

The format is picked by file extension. Text files that are not UTF-8 are read as Windows-1252.
Each converter run is killed after 120 s.

## API

One endpoint. Uploads are limited to 100 MB.

```sh
curl -F file=@report.pdf http://localhost:3000/documents
```

```json
{
  "filename": "report.pdf",
  "metadata": null,
  "markdown": "# Report\n\n...",
  "chunks": [
    { "heading_path": ["Report", "Results"], "content": "..." }
  ]
}
```

Each chunk is one heading section, at most `CHUNK_MAX_TOKENS` cl100k tokens (the tokenizer of OpenAI's
`text-embedding-3` models). Sections that are too long split at block boundaries, then at sentences;
long tables split by rows and repeat their header.

Errors come back as `{"error": "..."}`:

| Status | Cause |
|---|---|
| 400 | no file field in the multipart body |
| 415 | missing or unsupported file extension |
| 422 | file could not be converted, timed out, or contains no text |
| 500 | server fault (details only in the logs) |

Authentication and rate limiting are not handled here; put the service behind a gateway.

## Configuration

| Variable | Default | |
|---|---|---|
| `DATABASE_URL` | | full Postgres URL; if unset, the libpq variables below are used |
| `PGHOST`, `PGPORT`, `PGUSER`, `PGPASSWORD`, `PGDATABASE`, `PGSSLMODE` | `localhost:5432` | standard libpq variables |
| `CHUNK_MAX_TOKENS` | `512` | chunk budget; changing it re-chunks stored files on their next upload |
| `BIND_ADDR` | `0.0.0.0:3000` | listen address |
| `RUST_LOG` | `olympia=info,tower_http=info` | log filter |

Copy `.env.example` to `.env` to set them. Migrations run on start-up.

## Running

With Docker (Postgres included):

```sh
docker compose up --build
```

Locally you need Rust, Postgres, [pandoc](https://pandoc.org) 3.x, poppler (`pdftohtml`) and LibreOffice
(`soffice`), all on `PATH`:

```sh
brew install pandoc poppler libreoffice   # macOS
cargo run --release
```

## Testing

```sh
cargo test                  # unit tests
./samples/send_all.sh       # uploads every file in samples/ to a running server
```

`send_all.sh` takes an optional base URL and folder, prints status, chunk count and time per file,
saves each response to `samples/out/`, and exits non-zero if any upload failed. It needs `jq`.

## License

GPL-3.0, see [LICENSE](LICENSE).
