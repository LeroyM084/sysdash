mod stats;
mod updates;
mod web;

use std::{env, net::SocketAddr, process, time::Duration};

use axum::{Router, routing::get};

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

#[tokio::main(flavor = "current_thread")]
async fn main() {
    let config = Config::from_env().unwrap_or_else(|e| {
        eprintln!("sysdash: {e}");
        process::exit(1);
    });

    let app = Router::new().fallback(get(web::static_handler));

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
