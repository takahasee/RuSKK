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
    
    // サブコマンドが指定されている場合は、プロキシを起動せずに処理を実行して終了する
    match args.command {
        Some(ruskk::config::Command::ImportUserDict { path }) => {
            info!("Importing frequencies from {:?}", path);
            let mut predictor = FrequencyPredictor::new(history_path);
            if let Err(e) = predictor.import_from_skk_dict(&path) {
                tracing::error!("Failed to import from skk dict: {}", e);
                std::process::exit(1);
            }
            info!("Successfully imported frequencies and ensured context presets.");
            return Ok(());
        }
        Some(ruskk::config::Command::InitSeed { force }) => {
            let mut predictor = FrequencyPredictor::new(history_path.clone());
            if let Err(e) = predictor.init_seed(force) {
                tracing::error!("Failed to initialize seed file: {}", e);
                std::process::exit(1);
            }
            info!(
                path = ?history_path,
                force = force,
                "Successfully initialized/merged default context presets into seed file!"
            );
            return Ok(());
        }
        None => {}
    }

    let predictor = Arc::new(Mutex::new(FrequencyPredictor::new(history_path)));

    let proxy = Proxy {
        listen: args.listen.clone(),
        primary: args.primary(),
        fallback: args.fallback(),
        predictor: Arc::clone(&predictor),
        okuri_expansion: args.is_okuri_expansion_enabled(),
    };

    // SIGTERM / Ctrl-C を受け取ったらプロキシを停止する。
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

    info!("ruskk shutting down");
    Ok(())
}
