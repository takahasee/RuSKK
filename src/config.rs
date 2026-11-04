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

    /// Timeout for azoo-key-skkserv queries in ms (capped by 850ms overall deadline; 300ms/query in okuri expansion)
    #[arg(long, default_value_t = 600)]
    pub azookey_timeout_ms: u64,

    /// Timeout for yaskkserv2 queries in ms (capped by 850ms overall deadline; 200ms in okuri merge)
    #[arg(long, default_value_t = 700)]
    pub yaskkserv2_timeout_ms: u64,

    /// 送りあり見出し（例: かk -> かく）の活用復元を行うか（環境変数 RUSKKSERV_OKURI_EXPANSION でも制御可能） [default: true]
    #[arg(
        long,
        action = clap::ArgAction::Set,
        num_args = 0..=1,
        default_missing_value = "true"
    )]
    pub okuri_expansion: Option<bool>,

    /// 直前確定単語に基づく文脈共起並び替えを行うか（環境変数 RUSKKSERV_CONTEXT_RANKING でも制御可能） [default: true]
    #[arg(
        long,
        action = clap::ArgAction::Set,
        num_args = 0..=1,
        default_missing_value = "true"
    )]
    pub context_ranking: Option<bool>,

    /// システム辞書（大辞林等）に項目がある単語および SKK 注釈付き単語を最優先するか（環境変数 RUSKKSERV_ANNOTATION_PRIORITY でも制御可能） [default: true]
    #[arg(
        long,
        action = clap::ArgAction::Set,
        num_args = 0..=1,
        default_missing_value = "true"
    )]
    pub annotation_priority: Option<bool>,

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
        self.okuri_expansion
            .unwrap_or_else(|| parse_bool_env("RUSKKSERV_OKURI_EXPANSION", true))
    }

    pub fn is_context_ranking_enabled(&self) -> bool {
        self.context_ranking
            .unwrap_or_else(|| parse_bool_env("RUSKKSERV_CONTEXT_RANKING", true))
    }

    pub fn is_annotation_priority_enabled(&self) -> bool {
        self.annotation_priority
            .unwrap_or_else(|| parse_bool_env("RUSKKSERV_ANNOTATION_PRIORITY", true))
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_cli_bool_flags_rollback() {
        // デフォルトでは未指定（None）で、is_*_enabled() は環境変数がなければ true
        let default_args = Args::try_parse_from(["ruskkserv"]).unwrap();
        assert_eq!(default_args.azookey_timeout_ms, 600);
        assert_eq!(default_args.yaskkserv2_timeout_ms, 700);
        assert_eq!(default_args.okuri_expansion, None);
        assert_eq!(default_args.context_ranking, None);
        assert_eq!(default_args.annotation_priority, None);
        assert!(default_args.is_okuri_expansion_enabled());
        assert!(default_args.is_context_ranking_enabled());
        assert!(default_args.is_annotation_priority_enabled());

        // =false 指定で個別にロールバックできること（CLI 最優先）
        let disabled_args = Args::try_parse_from([
            "ruskkserv",
            "--okuri-expansion=false",
            "--context-ranking=false",
            "--annotation-priority=false",
        ])
        .unwrap();
        assert_eq!(disabled_args.okuri_expansion, Some(false));
        assert_eq!(disabled_args.context_ranking, Some(false));
        assert_eq!(disabled_args.annotation_priority, Some(false));
        assert!(!disabled_args.is_okuri_expansion_enabled());
        assert!(!disabled_args.is_context_ranking_enabled());
        assert!(!disabled_args.is_annotation_priority_enabled());

        // 値なしフラグ単体指定でも true として解釈されること
        let bare_flags = Args::try_parse_from([
            "ruskkserv",
            "--okuri-expansion",
            "--context-ranking",
            "--annotation-priority",
        ])
        .unwrap();
        assert_eq!(bare_flags.okuri_expansion, Some(true));
        assert_eq!(bare_flags.context_ranking, Some(true));
        assert_eq!(bare_flags.annotation_priority, Some(true));
        assert!(bare_flags.is_okuri_expansion_enabled());
        assert!(bare_flags.is_context_ranking_enabled());
        assert!(bare_flags.is_annotation_priority_enabled());
    }
}
