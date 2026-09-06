use std::sync::Arc;

use clap::Parser;
use tokio::sync::Mutex;
use tracing::info;
use tracing_subscriber::EnvFilter;

use ruskk::config::Args;
use ruskk::frequency::FrequencyPredictor;
use ruskk::proxy::Proxy;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .with_target(false)
        .init();

    let args = Args::parse();
    let history_path = std::env::var("HOME")
        .ok()
        .map(|h| std::path::PathBuf::from(h).join(".ruskk-frequency.json"));
    let predictor = Arc::new(Mutex::new(FrequencyPredictor::new(history_path)));

    let proxy = Proxy {
        listen: args.listen.clone(),
        primary: args.primary(),
        fallback: args.fallback(),
        predictor: Arc::clone(&predictor),
    };

    // SIGTERM / Ctrl-C を受け取ったらプロキシを停止し、学習データを保存する。
    // skk-bayesian.el の kill-emacs-hook 相当。
    let shutdown = async {
        #[cfg(unix)]
        {
            use tokio::signal::unix::{signal, SignalKind};
            let mut sigterm = signal(SignalKind::terminate()).expect("SIGTERM handler");
            let mut sigint  = signal(SignalKind::interrupt()).expect("SIGINT handler");
            tokio::select! {
                _ = sigterm.recv() => info!("received SIGTERM"),
                _ = sigint.recv()  => info!("received SIGINT"),
            }
        }
        #[cfg(not(unix))]
        {
            tokio::signal::ctrl_c().await.expect("ctrl-c handler");
            info!("received Ctrl-C");
        }
    };

    tokio::select! {
        result = proxy.run() => { result? }
        _ = shutdown => {}
    }

    // シャットダウン時に必ず学習データをディスクへ書き出す。
    // SAVE_INTERVAL に達していなくても確実に保存する。
    let guard = predictor.lock().await;
    match guard.save() {
        Ok(()) => info!("frequency data saved on shutdown"),
        Err(e) => tracing::warn!(error = %e, "failed to save frequency data on shutdown"),
    }

    Ok(())
}
