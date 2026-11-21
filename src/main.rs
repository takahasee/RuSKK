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
            let mut predictor = FrequencyPredictor::new_unloaded(history_path);
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
        Some(ruskkserv::config::Command::InitSeed {
            force,
            fasttext,
            dict,
            min_similarity,
            min_margin,
            max_per_target,
            output,
            send_reload,
        }) => {
            let target_path = output.or(history_path);
            let mut predictor = FrequencyPredictor::new_unloaded(target_path.clone());
            let fasttext_opts = fasttext.map(|vec_path| ruskkserv::fasttext::FastTextExpandOptions {
                vec_path,
                dict_path: dict,
                min_similarity,
                min_margin,
                max_per_target,
            });
            match predictor.init_seed_with_fasttext(force, fasttext_opts.as_ref()) {
                Ok(stats) => {
                    if let Some(s) = stats {
                        info!(
                            path = ?target_path,
                            force = force,
                            dimension = s.dimension,
                            matched_anchors = s.matched_anchors,
                            active_targets = s.active_targets,
                            scanned_words = s.scanned_words,
                            added_pairs = s.added_pairs,
                            added_context_keys = s.added_context_keys,
                            "Successfully initialized/merged presets and expanded with fastText!"
                        );
                    } else {
                        info!(
                            path = ?target_path,
                            force = force,
                            "Successfully initialized/merged default context presets into seed file!"
                        );
                    }
                }
                Err(e) => {
                    tracing::error!("Failed to initialize seed file: {}", e);
                    std::process::exit(1);
                }
            }
            if send_reload {
                send_sighup_to_ruskkserv();
            }
            return Ok(());
        }
        None => {}
    }

    if !args.listen.ip().is_loopback() {
        tracing::warn!(
            listen = %args.listen,
            "skkserv プロトコルには認証・暗号化がないため、ループバック以外への待受公開は非推奨です"
        );
    }

    let mut initial_predictor = FrequencyPredictor::new(history_path.clone());
    // seed ファイルが無い（インポート失敗を含む）場合でも文脈連動が効くよう、組み込みプリセットを適用する
    if initial_predictor.ensure_default_context_presets() {
        tracing::warn!(
            path = ?history_path,
            "context_frequencies is empty (seed file missing?) — using built-in default presets"
        );
    }
    info!(
        frequencies = initial_predictor.frequencies.len(),
        context_frequencies = initial_predictor.context_frequencies.len(),
        "predictor loaded"
    );
    let predictor = Arc::new(RwLock::new(initial_predictor));

    let lookup_cache = Arc::new(ruskkserv::cache::LookupCache::default());

    let proxy = Proxy {
        listen: args.listen.to_string(),
        azookey: args.azookey(),
        yaskkserv2: args.yaskkserv2(),
        backend_order: args.resolved_backend_order(),
        predictor: Arc::clone(&predictor),
        lookup_cache: Arc::clone(&lookup_cache),
        okuri_expansion: args.is_okuri_expansion_enabled(),
        context_ranking: args.is_context_ranking_enabled(),
        annotation_priority: args.is_annotation_priority_enabled(),
    };

    // SIGTERM / SIGINT を受け取ったらプロキシを停止する。
    let shutdown = async {
        use tokio::signal::unix::{SignalKind, signal};
        let mut sigterm = signal(SignalKind::terminate()).expect("SIGTERM handler");
        let mut sigint = signal(SignalKind::interrupt()).expect("SIGINT handler");
        tokio::select! {
            _ = sigterm.recv() => info!("received SIGTERM"),
            _ = sigint.recv()  => info!("received SIGINT"),
        }
    };

    // SIGHUP を受け取ったら ~/.ruskkserv-frequency.json をホットリロードし、変換キャッシュをクリアする。
    // import-user-dict --send-reload 実行後に predictor を再起動なしで更新するために使用する。
    let reload = reload_on_sighup(Arc::clone(&predictor), Arc::clone(&lookup_cache), history_path);

    tokio::select! {
        result = proxy.run() => { result? }
        _ = shutdown => {}
        _ = reload => {}
    }

    info!("ruskkserv shutting down");
    Ok(())
}

/// SIGHUP を受け取るたびに predictor を ~/.ruskkserv-frequency.json から再ロードし、
/// メモリ上の変換キャッシュを全消去して即座に最新状態を反映する。
/// このタスクは永久に実行され続け、プロセスが終了するまでシグナルを待ち受ける。
async fn reload_on_sighup(
    predictor: SharedPredictor,
    lookup_cache: Arc<ruskkserv::cache::LookupCache>,
    path: Option<PathBuf>,
) {
    use tokio::signal::unix::{SignalKind, signal};
    let mut sighup = signal(SignalKind::hangup()).expect("SIGHUP handler");
    loop {
        sighup.recv().await;
        info!("received SIGHUP — reloading predictor and clearing lookup cache");
        lookup_cache.clear();
        let mut guard = predictor.write().unwrap_or_else(|e| e.into_inner());
        if let Some(ref p) = path {
            match guard.load(p) {
                Ok(()) => info!(path = ?p, "predictor reloaded successfully"),
                Err(e) => tracing::warn!(error = %e, "predictor reload failed"),
            }
            if guard.ensure_default_context_presets() {
                tracing::warn!(
                    "context_frequencies is empty after reload — using built-in default presets"
                );
            }
        } else {
            tracing::warn!("SIGHUP received but no history_path configured — skipping reload");
        }
    }
}

/// インポート成功後に実行中の ruskkserv プロセスへ SIGHUP を送る。
/// PATH ハイジャックおよび他ユーザープロセス誤送信を防ぐため `/usr/bin/pkill` と `-u <uid> -x` を指定する。
fn send_sighup_to_ruskkserv() {
    let mut cmd = std::process::Command::new("/usr/bin/pkill");
    cmd.arg("-HUP");
    #[cfg(unix)]
    {
        unsafe extern "C" {
            fn geteuid() -> u32;
        }
        // SAFETY: geteuid() はスレッドセーフかつ失敗しない POSIX 関数で実効 UID を返す。
        let uid = unsafe { geteuid() };
        cmd.args(["-u", &uid.to_string()]);
    }
    cmd.args(["-x", "ruskkserv"]);

    match cmd.status() {
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
