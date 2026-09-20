use std::net::SocketAddr;
use std::time::Duration;

use clap::Parser;

use crate::backend::{Backend, UpstreamEncoding};

#[derive(Debug, Parser)]
#[command(
    name = "ruskkserv",
    about = "skkserv proxy: azoo-key-skkserv first, yaskkserv2 fallback",
    version
)]
pub struct Args {
    /// Address to listen on (macSKK connects here)
    #[arg(long, default_value = "127.0.0.1:1178")]
    pub listen: String,

    /// azoo-key-skkserv address (primary)
    #[arg(long, default_value = "127.0.0.1:1180")]
    pub azookey: SocketAddr,

    /// yaskkserv2 address (fallback)
    #[arg(long, default_value = "127.0.0.1:1179")]
    pub yaskkserv2: SocketAddr,

    /// Timeout for azoo-key-skkserv queries (milliseconds)
    #[arg(long, default_value_t = 1500)]
    pub azookey_timeout_ms: u64,

    /// Timeout for yaskkserv2 queries (milliseconds)
    #[arg(long, default_value_t = 700)]
    pub yaskkserv2_timeout_ms: u64,

    /// 送りあり見出し（例: かk -> かく）の活用復元を行うか（環境変数 RUSKKSERV_OKURI_EXPANSION でも制御可能）
    #[arg(long, default_value_t = true)]
    pub okuri_expansion: bool,

    /// 直前確定単語に基づく文脈共起並び替えを行うか（環境変数 RUSKKSERV_CONTEXT_RANKING でも制御可能）
    #[arg(long, default_value_t = true)]
    pub context_ranking: bool,

    #[command(subcommand)]
    pub command: Option<Command>,
}

#[derive(Debug, clap::Subcommand)]
pub enum Command {
    /// Import frequency data from a macSKK user dictionary
    ImportUserDict {
        /// Path to macSKK user dictionary (e.g., skk-jisyo.utf8)
        path: std::path::PathBuf,
        /// インポート成功後に実行中の ruskkserv プロセスに SIGHUP を送りホットリロードを行う
        #[arg(long, default_value_t = false)]
        send_reload: bool,
    },
    /// Initialize or merge default context co-occurrence presets into ~/.ruskkserv-frequency.json
    InitSeed {
        /// Overwrite completely with presets instead of merging with existing data
        #[arg(long, short)]
        force: bool,
    },
}

impl Args {
    pub fn primary(&self) -> Backend {
        Backend::new(
            "azoo-key-skkserv",
            self.azookey,
            UpstreamEncoding::EucJpRequestUtf8Response,
            Duration::from_millis(self.azookey_timeout_ms),
        )
    }

    pub fn fallback(&self) -> Backend {
        Backend::new(
            "yaskkserv2",
            self.yaskkserv2,
            UpstreamEncoding::EucJp,
            Duration::from_millis(self.yaskkserv2_timeout_ms),
        )
    }

    pub fn is_okuri_expansion_enabled(&self) -> bool {
        parse_bool_env("RUSKKSERV_OKURI_EXPANSION", self.okuri_expansion)
    }

    pub fn is_context_ranking_enabled(&self) -> bool {
        parse_bool_env("RUSKKSERV_CONTEXT_RANKING", self.context_ranking)
    }
}

fn parse_bool_env(var: &str, default: bool) -> bool {
    if let Ok(val) = std::env::var(var) {
        let val = val.trim().to_lowercase();
        if matches!(val.as_str(), "0" | "false" | "no" | "off") {
            return false;
        }
        if matches!(val.as_str(), "1" | "true" | "yes" | "on") {
            return true;
        }
    }
    default
}

