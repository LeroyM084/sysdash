mod stats;
mod updates;
mod web;

use std::{convert::Infallible, env, net::SocketAddr, process, sync::Arc, time::Duration};

use axum::{
    Json, Router,
    extract::State,
    http::StatusCode,
    response::{
        IntoResponse, Response,
        sse::{Event, KeepAlive, Sse},
    },
    routing::{get, post},
};
use futures_util::stream::{self, Stream};
use tokio::sync::watch;
use updates::{AptBackend, MockBackend, UpdateBackend, UpdateError, UpdateList, UpdateStatus};

struct Config {
    bind: SocketAddr,
    interval: Duration,
    mock_updates: bool,
}

impl Config {
    fn from_env() -> Result<Self, String> {
        let bind = env::var("SYSDASH_BIND").unwrap_or_else(|_| "127.0.0.1:8090".into());
        let bind = bind
            .parse()
            .map_err(|e| format!("SYSDASH_BIND invalide ({bind}): {e}"))?;

        let interval = env::var("SYSDASH_INTERVAL_MS").unwrap_or_else(|_| "1500".into());
        let interval = match interval.parse::<u64>() {
            Ok(ms) if ms > 0 => Duration::from_millis(ms),
            _ => return Err(format!("SYSDASH_INTERVAL_MS invalide ({interval})")),
        };

        let mock_updates = match env::var("SYSDASH_MOCK_UPDATES").as_deref() {
            Ok("1") => true,
            Ok("0") | Err(_) => false,
            Ok(other) => return Err(format!("SYSDASH_MOCK_UPDATES invalide ({other})")),
        };

        Ok(Self {
            bind,
            interval,
            mock_updates,
        })
    }
}

#[derive(Clone)]
struct AppState {
    stats: watch::Receiver<Arc<str>>,
    updates: Arc<dyn UpdateBackend>,
}

/// Chaque client relaie le dernier snapshot déjà sérialisé : aucun calcul par client.
async fn stream_stats(
    State(state): State<AppState>,
) -> Sse<impl Stream<Item = Result<Event, Infallible>>> {
    let mut rx = state.stats;
    rx.mark_changed();
    let events = stream::unfold(rx, |mut rx| async move {
        rx.changed().await.ok()?;
        let json = rx.borrow_and_update().clone();
        Some((Ok(Event::default().data(&*json)), rx))
    });
    Sse::new(events).keep_alive(KeepAlive::default())
}

fn error_response(status: StatusCode, message: impl Into<String>) -> Response {
    let body = serde_json::json!({ "error": message.into() });
    (status, Json(body)).into_response()
}

/// Exécute un appel (bloquant) au backend de MAJ hors du thread du runtime.
async fn call_backend<T, F>(state: &AppState, f: F) -> Result<T, Response>
where
    T: Send + 'static,
    F: FnOnce(&dyn UpdateBackend) -> Result<T, UpdateError> + Send + 'static,
{
    let backend = Arc::clone(&state.updates);
    match tokio::task::spawn_blocking(move || f(backend.as_ref())).await {
        Ok(Ok(value)) => Ok(value),
        Ok(Err(UpdateError::AlreadyRunning)) => Err(error_response(
            StatusCode::CONFLICT,
            "une mise à jour est déjà en cours",
        )),
        Ok(Err(UpdateError::Failed(e))) => {
            eprintln!("sysdash: {e}");
            Err(error_response(StatusCode::INTERNAL_SERVER_ERROR, e))
        }
        Err(e) => Err(error_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            e.to_string(),
        )),
    }
}

async fn list_updates(State(state): State<AppState>) -> Result<Json<UpdateList>, Response> {
    call_backend(&state, |b| b.list()).await.map(Json)
}

async fn apply_updates(State(state): State<AppState>) -> Result<StatusCode, Response> {
    call_backend(&state, |b| b.apply())
        .await
        .map(|()| StatusCode::ACCEPTED)
}

async fn update_status(State(state): State<AppState>) -> Result<Json<UpdateStatus>, Response> {
    call_backend(&state, |b| b.status()).await.map(Json)
}

#[tokio::main(flavor = "current_thread")]
async fn main() {
    let config = Config::from_env().unwrap_or_else(|e| {
        eprintln!("sysdash: {e}");
        process::exit(1);
    });

    let updates: Arc<dyn UpdateBackend> = if config.mock_updates {
        Arc::new(MockBackend::default())
    } else {
        Arc::new(AptBackend::default())
    };
    let state = AppState {
        stats: stats::spawn_collector(config.interval).await,
        updates,
    };

    let app = Router::new()
        .route("/api/stream", get(stream_stats))
        .route("/api/updates", get(list_updates))
        .route("/api/updates/apply", post(apply_updates))
        .route("/api/updates/status", get(update_status))
        .fallback(get(web::static_handler))
        .with_state(state);

    let listener = tokio::net::TcpListener::bind(config.bind)
        .await
        .unwrap_or_else(|e| {
            eprintln!("sysdash: impossible d'écouter sur {}: {e}", config.bind);
            process::exit(1);
        });

    eprintln!(
        "sysdash: écoute sur {} (intervalle {} ms, mises à jour {})",
        config.bind,
        config.interval.as_millis(),
        if config.mock_updates { "mock" } else { "apt" },
    );

    if let Err(e) = axum::serve(listener, app).await {
        eprintln!("sysdash: erreur serveur: {e}");
        process::exit(1);
    }
}
