use std::sync::Arc;

use clap::Parser;
use tokio::sync::Mutex;
use tracing_subscriber::EnvFilter;

use skk_proxy::config::Args;
use skk_proxy::frequency::FrequencyPredictor;
use skk_proxy::proxy::Proxy;

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
        .map(|h| std::path::PathBuf::from(h).join(".skk-proxy-frequency.json"));
    let predictor = Arc::new(Mutex::new(FrequencyPredictor::new(history_path)));

    let proxy = Proxy {
        listen: args.listen.clone(),
        primary: args.primary(),
        fallback: args.fallback(),
        predictor,
    };

    proxy.run().await
}
