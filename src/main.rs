mod chunk;
mod convert;
mod error;
mod ocr;
mod pdf;

use axum::{
    Json, Router,
    extract::{DefaultBodyLimit, Multipart, State},
    routing::post,
};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use sqlx::{PgPool, postgres::PgConnectOptions};
use tower_http::trace::{DefaultMakeSpan, DefaultOnResponse, TraceLayer};
use tracing::{Level, info, instrument};
use tracing_subscriber::EnvFilter;
use uuid::Uuid;

use error::AppError;

/// A stored conversion: id, metadata, markdown, chunks, and the chunk budget they were cut with.
type Stored = (Uuid, Option<Value>, String, Value, Option<i32>);

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| "olympia=info,tower_http=info".into()))
        .init();

    // DATABASE_URL wins; otherwise the standard libpq variables (PGHOST, PGPORT, PGUSER,
    // PGPASSWORD, PGDATABASE, PGSSLMODE, ...) are read, defaulting to localhost:5432.
    let options = match std::env::var("DATABASE_URL") {
        Ok(url) => url.parse()?,
        Err(_) => PgConnectOptions::new(),
    };
    let pool = PgPool::connect_with(options).await?;
    sqlx::migrate!().run(&pool).await?;
    // Read now so a bad CHUNK_MAX_TOKENS fails at boot, not on the first upload.
    info!(max_tokens = *chunk::MAX_TOKENS, "chunk budget");
    // The tokenizer loads lazily (~0.3s); do it now so the first upload doesn't pay for it.
    tiktoken_rs::cl100k_base_singleton();
    convert::warm_up().await;

    let app = Router::new()
        .route("/documents", post(upload))
        .layer(DefaultBodyLimit::max(100 * 1024 * 1024))
        .layer(
            TraceLayer::new_for_http()
                .make_span_with(DefaultMakeSpan::new().level(Level::INFO))
                .on_response(DefaultOnResponse::new().level(Level::INFO)),
        )
        .with_state(pool);

    let addr = std::env::var("BIND_ADDR").unwrap_or_else(|_| "0.0.0.0:3000".into());
    let listener = tokio::net::TcpListener::bind(&addr).await?;
    info!(%addr, "listening");
    axum::serve(listener, app).await?;
    Ok(())
}

#[instrument(skip_all)]
async fn upload(State(pool): State<PgPool>, mut multipart: Multipart) -> Result<Json<Value>, AppError> {
    while let Some(field) = multipart.next_field().await? {
        let Some(filename) = field.file_name().map(str::to_string) else { continue };
        let content_type = field.content_type().map(str::to_string);
        let data = field.bytes().await?;

        // Same bytes were processed before: hand back the stored result.
        let hash = Sha256::digest(&data).to_vec();
        let budget = *chunk::MAX_TOKENS as i32;
        // Hits answer with the caller's own filename: the first uploader's name and id stay private.
        if let Some((id, metadata, markdown, chunks, stored_budget)) = by_hash(&pool, &hash).await? {
            if stored_budget == Some(budget) {
                info!(%id, filename, "duplicate upload, returning stored result");
                return Ok(respond(&filename, metadata, markdown, chunks));
            }
            // Chunk budget changed since: re-chunk the stored markdown, still skipping conversion.
            let chunks = chunk::chunk(&markdown);
            sqlx::query("UPDATE documents SET chunks = $1, chunk_tokens = $2 WHERE id = $3")
                .bind(sqlx::types::Json(&chunks))
                .bind(budget)
                .bind(id)
                .execute(&pool)
                .await?;
            info!(%id, filename, chunks = chunks.len(), "duplicate upload, re-chunked stored markdown");
            return Ok(respond(&filename, metadata, markdown, json!(chunks)));
        }

        let convert::Converted { markdown, metadata } = convert::to_markdown(&filename, &data).await?;
        let chunks = chunk::chunk(&markdown);

        // A concurrent upload of the same file may have won the race; then this result is not stored.
        let inserted: Option<(Uuid,)> = sqlx::query_as(
            "INSERT INTO documents (filename, content_type, data, markdown, chunks, metadata, chunk_tokens)
             VALUES ($1, $2, $3, $4, $5, $6, $7) ON CONFLICT (sha256) DO NOTHING RETURNING id",
        )
        .bind(&filename)
        .bind(&content_type)
        .bind(&data[..])
        .bind(&markdown)
        .bind(sqlx::types::Json(&chunks))
        .bind(metadata.as_ref().map(sqlx::types::Json))
        .bind(budget)
        .fetch_optional(&pool)
        .await?;

        match inserted {
            Some((id,)) => info!(%id, filename, bytes = data.len(), chunks = chunks.len(), "document stored"),
            None => info!(filename, "concurrent duplicate upload, result not stored"),
        }
        return Ok(respond(&filename, metadata, markdown, json!(chunks)));
    }
    Err(AppError::MissingFile)
}

/// The stored conversion of these bytes, if any.
async fn by_hash(pool: &PgPool, hash: &[u8]) -> Result<Option<Stored>, sqlx::Error> {
    sqlx::query_as("SELECT id, metadata, markdown, chunks, chunk_tokens FROM documents WHERE sha256 = $1")
        .bind(hash)
        .fetch_optional(pool)
        .await
}

fn respond(filename: &str, metadata: Option<Value>, markdown: String, chunks: Value) -> Json<Value> {
    Json(json!({ "filename": filename, "metadata": metadata, "markdown": markdown, "chunks": chunks }))
}
