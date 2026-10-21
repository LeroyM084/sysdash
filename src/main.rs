mod stats;
mod updates;
mod web;

use std::{convert::Infallible, env, net::SocketAddr, process, sync::Arc, time::Duration};

use axum::{
    Router,
    extract::State,
    response::sse::{Event, KeepAlive, Sse},
    routing::get,
};
use futures_util::stream::{self, Stream};
use tokio::sync::watch;

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

/// Chaque client relaie le dernier snapshot déjà sérialisé : aucun calcul par client.
async fn stream_stats(
    State(mut rx): State<watch::Receiver<Arc<str>>>,
) -> Sse<impl Stream<Item = Result<Event, Infallible>>> {
    rx.mark_changed();
    let events = stream::unfold(rx, |mut rx| async move {
        rx.changed().await.ok()?;
        let json = rx.borrow_and_update().clone();
        Some((Ok(Event::default().data(&*json)), rx))
    });
    Sse::new(events).keep_alive(KeepAlive::default())
}

#[tokio::main(flavor = "current_thread")]
async fn main() {
    let config = Config::from_env().unwrap_or_else(|e| {
        eprintln!("sysdash: {e}");
        process::exit(1);
    });

    let stats = stats::spawn_collector(config.interval).await;

    let app = Router::new()
        .route("/api/stream", get(stream_stats))
        .fallback(get(web::static_handler))
        .with_state(stats);

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
