use std::path::PathBuf;
use std::sync::{Arc, RwLock};

use clap::Parser;
use tracing::info;
use tracing_subscriber::EnvFilter;

use ruskkserv::config::Args;
use ruskkserv::frequency::{FrequencyPredictor, SharedPredictor};
use ruskkserv::proxy::Proxy;

#[global_allocator]
static GLOBAL: mimalloc::MiMalloc = mimalloc::MiMalloc;

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
        .map(|h| PathBuf::from(h).join(".ruskkserv-frequency.json"));
    
    // サブコマンドが指定されている場合は、プロキシを起動せずに処理を実行して終了する
    match args.command {
        Some(ruskkserv::config::Command::ImportUserDict { path, send_reload }) => {
            info!("Importing frequencies from {:?}", path);
            let mut predictor = FrequencyPredictor::new(history_path);
            if let Err(e) = predictor.import_from_skk_dict(&path) {
                tracing::error!("Failed to import from skk dict: {}", e);
                std::process::exit(1);
            }
            info!("Successfully imported frequencies and ensured context presets.");
            // --send-reload が指定された場合、実行中の ruskkserv プロセスに SIGHUP を送りホットリロードを行う
            if send_reload {
                send_sighup_to_ruskkserv();
            }
            return Ok(());
        }
        Some(ruskkserv::config::Command::InitSeed { force }) => {
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

    let predictor = Arc::new(RwLock::new(FrequencyPredictor::new(history_path.clone())));

    let proxy = Proxy {
        listen: args.listen.clone(),
        primary: args.primary(),
        fallback: args.fallback(),
        predictor: Arc::clone(&predictor),
        okuri_expansion: args.is_okuri_expansion_enabled(),
        context_ranking: args.is_context_ranking_enabled(),
    };

    // SIGTERM / SIGINT を受け取ったらプロキシを停止する。
    let shutdown = async {
        use tokio::signal::unix::{signal, SignalKind};
        let mut sigterm = signal(SignalKind::terminate()).expect("SIGTERM handler");
        let mut sigint  = signal(SignalKind::interrupt()).expect("SIGINT handler");
        tokio::select! {
            _ = sigterm.recv() => info!("received SIGTERM"),
            _ = sigint.recv()  => info!("received SIGINT"),
        }
    };

    // SIGHUP を受け取ったら ~/.ruskkserv-frequency.json をホットリロードする。
    // import-user-dict --send-reload 実行後に predictor を再起動なしで更新するために使用する。
    let reload = reload_on_sighup(Arc::clone(&predictor), history_path);

    tokio::select! {
        result = proxy.run() => { result? }
        _ = shutdown => {}
        _ = reload => {}
    }

    info!("ruskkserv shutting down");
    Ok(())
}

/// SIGHUP を受け取るたびに predictor を ~/.ruskkserv-frequency.json から再ロードする。
/// このタスクは永久に実行され続け、プロセスが終了するまでシグナルを待ち受ける。
async fn reload_on_sighup(predictor: SharedPredictor, path: Option<PathBuf>) {
    use tokio::signal::unix::{signal, SignalKind};
    let mut sighup = signal(SignalKind::hangup()).expect("SIGHUP handler");
    loop {
        sighup.recv().await;
        info!("received SIGHUP — reloading predictor");
        let mut guard = predictor.write().unwrap_or_else(|e| e.into_inner());
        if let Some(ref p) = path {
            match guard.load(p) {
                Ok(()) => info!(path = ?p, "predictor reloaded successfully"),
                Err(e) => tracing::warn!(error = %e, "predictor reload failed"),
            }
        } else {
            tracing::warn!("SIGHUP received but no history_path configured — skipping reload");
        }
    }
}

/// インポート成功後に実行中の ruskkserv プロセスへ SIGHUP を送る。
/// macOS の pkill コマンドを使用する（`-x` で完全一致、誤送信防止）。
fn send_sighup_to_ruskkserv() {
    match std::process::Command::new("pkill")
        .args(["-HUP", "-x", "ruskkserv"])
        .status()
    {
        Ok(status) if status.success() => {
            info!("sent SIGHUP to ruskkserv — predictor hot-reload triggered");
        }
        Ok(status) => {
            // 終了コード 1 = プロセスが見つからない（ruskkserv が起動していない場合）
            tracing::debug!(%status, "pkill returned non-zero (ruskkserv may not be running)");
        }
        Err(e) => {
            tracing::warn!(error = %e, "failed to run pkill — SIGHUP not sent");
        }
    }
}
