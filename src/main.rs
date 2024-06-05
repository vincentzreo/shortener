use core::fmt;

use axum::{
    extract::{Path, State},
    http::{header::LOCATION, HeaderMap, StatusCode},
    response::IntoResponse,
    routing::{get, post},
    Json, Router,
};
use nanoid::nanoid;
use serde::{Deserialize, Serialize};
use sqlx::PgPool;
use thiserror::Error;
use tokio::net::TcpListener;
use tracing::{info, level_filters::LevelFilter};
use tracing_subscriber::{
    fmt::Layer, layer::SubscriberExt as _, util::SubscriberInitExt as _, Layer as _,
};

#[derive(Debug)]
struct StatusCodeError(StatusCode);
impl fmt::Display for StatusCodeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Status code error: {}", self.0)
    }
}
impl std::error::Error for StatusCodeError {}

#[derive(Debug, Error)]
enum ShortenError {
    #[error("Sql error: {0}")]
    SqlError(#[from] sqlx::Error),
    #[error("Io error: {0}")]
    IoError(#[from] std::io::Error),
    #[error("Axum error: {0}")]
    StatusCode(#[from] StatusCodeError),
}

impl IntoResponse for ShortenError {
    fn into_response(self) -> axum::response::Response {
        let status = match self {
            ShortenError::SqlError(_) => StatusCode::INTERNAL_SERVER_ERROR,
            ShortenError::IoError(_) => StatusCode::INTERNAL_SERVER_ERROR,
            ShortenError::StatusCode(e) => e.0,
        };
        (status, format!("{}", status)).into_response()
    }
}

#[derive(Debug, Serialize, Deserialize)]
struct ShortReq {
    url: String,
}

#[derive(Debug, Serialize, Deserialize)]
struct ShortRes {
    url: String,
}

#[derive(Debug, Clone)]
struct PgState {
    db: PgPool,
}

#[derive(Debug, sqlx::FromRow)]
struct Records {
    #[sqlx(default)]
    id: String,
    #[sqlx(default)]
    url: String,
}

const LISTEN_ADDR: &str = "0.0.0.0:8080";
const DB_URL: &str = "postgres://postgres:postgres@localhost/shortener";

#[tokio::main]
async fn main() -> Result<(), ShortenError> {
    let layer = Layer::new().pretty().with_filter(LevelFilter::INFO);
    tracing_subscriber::registry().with(layer).init();

    let state = PgState::try_new(DB_URL).await?;
    info!("Connected to database {}", DB_URL);

    let listener = TcpListener::bind(LISTEN_ADDR).await?;
    info!("Listening on: {}", LISTEN_ADDR);
    let router = Router::new()
        .route("/", post(shorten))
        .route("/:id", get(redirect))
        .with_state(state);
    axum::serve(listener, router.into_make_service()).await?;
    Ok(())
}

async fn shorten(
    State(state): State<PgState>,
    Json(req): Json<ShortReq>,
) -> Result<impl IntoResponse, ShortenError> {
    let id = state
        .shorten(&req.url)
        .await
        .map_err(|_| StatusCodeError(StatusCode::UNPROCESSABLE_ENTITY))?;
    let body = Json(ShortRes {
        url: format!("http://{}/{}", LISTEN_ADDR, id),
    });
    Ok((StatusCode::CREATED, body))
}

async fn redirect(
    State(state): State<PgState>,
    Path(id): Path<String>,
) -> Result<impl IntoResponse, ShortenError> {
    let url = state
        .get_url(&id)
        .await
        .map_err(|_| StatusCodeError(StatusCode::NOT_FOUND))?;
    let mut header = HeaderMap::new();
    header.insert(LOCATION, url.parse().unwrap());
    Ok((StatusCode::FOUND, header))
}

impl PgState {
    async fn try_new(db_url: &str) -> Result<Self, ShortenError> {
        let db = PgPool::connect(db_url).await?;
        sqlx::query("CREATE TABLE IF NOT EXISTS urls (id VARCHAR(6), url TEXT NOT NULL UNIQUE)")
            .execute(&db)
            .await?;
        Ok(Self { db })
    }
    async fn shorten(&self, url: &str) -> Result<String, ShortenError> {
        let mut id = nanoid!(6);
        let mut flag: (i64,) = sqlx::query_as("SELECT COUNT(id) FROM URLS WHERE id = $1")
            .bind(&id)
            .fetch_one(&self.db)
            .await?;
        while flag.0 == 1 {
            id = nanoid!(6);
            flag = sqlx::query_as("SELECT COUNT(id) FROM URLS WHERE id = $1")
                .bind(&id)
                .fetch_one(&self.db)
                .await?;
        }
        let ret: Records = sqlx::query_as("INSERT INTO urls (id, url) VALUES ($1, $2) ON CONFLICT(url) DO UPDATE SET url = EXCLUDED.url RETURNING id")
            .bind(&id)
            .bind(url)
            .fetch_one(&self.db)
            .await?;
        Ok(ret.id)
    }
    async fn get_url(&self, id: &str) -> Result<String, ShortenError> {
        let row: Records = sqlx::query_as("SELECT url FROM urls WHERE id = $1")
            .bind(id)
            .fetch_one(&self.db)
            .await?;
        Ok(row.url)
    }
}
