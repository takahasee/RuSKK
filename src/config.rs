use std::net::SocketAddr;
use std::time::Duration;

use clap::Parser;

use crate::backend::{Backend, UpstreamEncoding};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, clap::ValueEnum)]
pub enum BackendOrder {
    /// yaskkserv2 を優先し、ミス・失敗時のみ azoo-key-skkserv へフォールバック（デフォルト）
    #[default]
    Yaskkserv2First,
    /// azoo-key-skkserv を優先し、ミス・失敗時のみ yaskkserv2 へフォールバック（従来順）
    AzookeyFirst,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, clap::ValueEnum)]
pub enum Yaskkserv2Encoding {
    /// EUC-JP（標準の `yaskkserv2` 既定設定）
    #[default]
    Euc,
    /// UTF-8（`yaskkserv2_make_dictionary --utf8` + `yaskkserv2 --midashi-utf8` 構成用）
    Utf8,
    /// リクエスト EUC-JP / レスポンス UTF-8（`--utf8` 辞書かつ `--midashi-utf8` なし構成用）
    EucReqUtf8Resp,
}

#[derive(Debug, Parser)]
#[command(
    name = "ruskkserv",
    about = "skkserv proxy: yaskkserv2 primary, azoo-key-skkserv fallback (configurable)",
    version
)]
pub struct Args {
    /// Address to listen on (macSKK connects here)
    #[arg(long, default_value = "127.0.0.1:1178")]
    pub listen: SocketAddr,

    /// azoo-key-skkserv address
    #[arg(long, default_value = "127.0.0.1:1180")]
    pub azookey: SocketAddr,

    /// yaskkserv2 address
    #[arg(long, default_value = "127.0.0.1:1179")]
    pub yaskkserv2: SocketAddr,

    /// yaskkserv2 との通信エンコーディング（euc [デフォルト] / utf8 / euc-req-utf8-resp; 環境変数 RUSKKSERV_YASKKSERV2_ENCODING でも制御可能）
    #[arg(long, value_enum)]
    pub yaskkserv2_encoding: Option<Yaskkserv2Encoding>,

    /// バックエンドの参照順（yaskkserv2-first [デフォルト] / azookey-first; 環境変数 RUSKKSERV_BACKEND_ORDER でも制御可能）
    #[arg(long, value_enum)]
    pub backend_order: Option<BackendOrder>,

    /// Timeout for azoo-key-skkserv queries in ms (capped by 850ms overall deadline; 300ms/query in okuri expansion)
    #[arg(long, default_value_t = 600)]
    pub azookey_timeout_ms: u64,

    /// Timeout for yaskkserv2 queries in ms (capped by 850ms overall deadline; 200ms in okuri merge)
    #[arg(long, default_value_t = 850)]
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
        /// fastText .vec file path to automatically expand context co-occurrence pairs
        #[arg(long)]
        fasttext: Option<std::path::PathBuf>,
        /// Optional SKK dictionary path (UTF-8 / EUC-JP) used to filter valid words and homophone groups
        #[arg(long)]
        dict: Option<std::path::PathBuf>,
        /// Minimum cosine similarity threshold for fastText expansion
        #[arg(long, default_value_t = 0.58)]
        min_similarity: f32,
        /// Minimum margin between top homophone targets for fastText expansion
        #[arg(long, default_value_t = 0.04)]
        min_margin: f32,
        /// Maximum number of new context words added per target candidate
        #[arg(long, default_value_t = 50)]
        max_per_target: usize,
        /// Also expand non-competing single-target entries (by default only homophone competing groups are expanded)
        #[arg(long, default_value_t = false)]
        include_non_competing: bool,
        /// Output seed JSON path (defaults to ~/.ruskkserv-frequency.json)
        #[arg(long, short)]
        output: Option<std::path::PathBuf>,
        /// Send SIGHUP to running ruskkserv process after updating the seed file
        #[arg(long, default_value_t = false)]
        send_reload: bool,
    },
}

impl Args {
    pub fn azookey(&self) -> Backend {
        Backend::new(
            "azoo-key-skkserv",
            self.azookey,
            UpstreamEncoding::EucJpRequestUtf8Response,
            Duration::from_millis(self.azookey_timeout_ms),
        )
    }

    pub fn yaskkserv2(&self) -> Backend {
        let encoding = match self.resolved_yaskkserv2_encoding() {
            Yaskkserv2Encoding::Euc => UpstreamEncoding::EucJp,
            Yaskkserv2Encoding::Utf8 => UpstreamEncoding::Utf8,
            Yaskkserv2Encoding::EucReqUtf8Resp => UpstreamEncoding::EucJpRequestUtf8Response,
        };
        Backend::new(
            "yaskkserv2",
            self.yaskkserv2,
            encoding,
            Duration::from_millis(self.yaskkserv2_timeout_ms),
        )
    }

    pub fn resolved_yaskkserv2_encoding(&self) -> Yaskkserv2Encoding {
        if let Some(encoding) = self.yaskkserv2_encoding {
            return encoding;
        }
        if let Ok(val) = std::env::var("RUSKKSERV_YASKKSERV2_ENCODING") {
            let norm = val.trim().to_ascii_lowercase();
            match norm.as_str() {
                "utf8" | "utf-8" => return Yaskkserv2Encoding::Utf8,
                "euc-req-utf8-resp" | "euc_req_utf8_resp" => {
                    return Yaskkserv2Encoding::EucReqUtf8Resp;
                }
                "euc" | "euc-jp" | "euc_jp" => return Yaskkserv2Encoding::Euc,
                _ => {}
            }
        }
        Yaskkserv2Encoding::Euc
    }

    pub fn resolved_backend_order(&self) -> BackendOrder {
        if let Some(order) = self.backend_order {
            return order;
        }
        if let Ok(val) = std::env::var("RUSKKSERV_BACKEND_ORDER") {
            let norm = val.trim().to_ascii_lowercase();
            if matches!(norm.as_str(), "azookey" | "azookey-first" | "azookey_first") {
                return BackendOrder::AzookeyFirst;
            }
            if matches!(
                norm.as_str(),
                "yaskkserv2" | "yaskkserv2-first" | "yaskkserv2_first"
            ) {
                return BackendOrder::Yaskkserv2First;
            }
        }
        BackendOrder::Yaskkserv2First
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
        assert_eq!(default_args.yaskkserv2_timeout_ms, 850);
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

        // デフォルトの listen アドレスが 127.0.0.1:1178 で、不正なホスト名は起動時に拒否されること
        assert_eq!(default_args.listen, "127.0.0.1:1178".parse::<SocketAddr>().unwrap());
        assert!(Args::try_parse_from(["ruskkserv", "--listen", "not-an-addr"]).is_err());

        // backend_order のデフォルトと CLI 上書き検証
        assert_eq!(default_args.backend_order, None);
        assert_eq!(
            default_args.resolved_backend_order(),
            BackendOrder::Yaskkserv2First
        );
        let azoo_first = Args::try_parse_from(["ruskkserv", "--backend-order=azookey-first"]).unwrap();
        assert_eq!(azoo_first.backend_order, Some(BackendOrder::AzookeyFirst));
        assert_eq!(
            azoo_first.resolved_backend_order(),
            BackendOrder::AzookeyFirst
        );
        let yaskk_first =
            Args::try_parse_from(["ruskkserv", "--backend-order=yaskkserv2-first"]).unwrap();
        assert_eq!(
            yaskk_first.resolved_backend_order(),
            BackendOrder::Yaskkserv2First
        );
    }

    #[test]
    fn test_yaskkserv2_encoding_cli_options() {
        let default_args = Args::try_parse_from(["ruskkserv"]).unwrap();
        assert_eq!(default_args.yaskkserv2_encoding, None);
        assert_eq!(
            default_args.resolved_yaskkserv2_encoding(),
            Yaskkserv2Encoding::Euc
        );
        assert_eq!(default_args.yaskkserv2().encoding, UpstreamEncoding::EucJp);

        let utf8_args =
            Args::try_parse_from(["ruskkserv", "--yaskkserv2-encoding=utf8"]).unwrap();
        assert_eq!(utf8_args.yaskkserv2_encoding, Some(Yaskkserv2Encoding::Utf8));
        assert_eq!(
            utf8_args.resolved_yaskkserv2_encoding(),
            Yaskkserv2Encoding::Utf8
        );
        assert_eq!(utf8_args.yaskkserv2().encoding, UpstreamEncoding::Utf8);

        let hybrid_args =
            Args::try_parse_from(["ruskkserv", "--yaskkserv2-encoding=euc-req-utf8-resp"]).unwrap();
        assert_eq!(
            hybrid_args.yaskkserv2_encoding,
            Some(Yaskkserv2Encoding::EucReqUtf8Resp)
        );
        assert_eq!(
            hybrid_args.resolved_yaskkserv2_encoding(),
            Yaskkserv2Encoding::EucReqUtf8Resp
        );
        assert_eq!(
            hybrid_args.yaskkserv2().encoding,
            UpstreamEncoding::EucJpRequestUtf8Response
        );

        let euc_args = Args::try_parse_from(["ruskkserv", "--yaskkserv2-encoding=euc"]).unwrap();
        assert_eq!(
            euc_args.resolved_yaskkserv2_encoding(),
            Yaskkserv2Encoding::Euc
        );
        assert_eq!(euc_args.yaskkserv2().encoding, UpstreamEncoding::EucJp);
    }

    #[test]
    fn test_init_seed_subcommand_args() {
        let minimal = Args::try_parse_from(["ruskkserv", "init-seed"]).unwrap();
        match minimal.command {
            Some(Command::InitSeed {
                force,
                fasttext,
                dict,
                min_similarity,
                min_margin,
                max_per_target,
                include_non_competing,
                output,
                send_reload,
            }) => {
                assert!(!force);
                assert!(fasttext.is_none());
                assert!(dict.is_none());
                assert!((min_similarity - 0.58).abs() < f32::EPSILON);
                assert!((min_margin - 0.04).abs() < f32::EPSILON);
                assert_eq!(max_per_target, 50);
                assert!(!include_non_competing);
                assert!(output.is_none());
                assert!(!send_reload);
            }
            _ => panic!("expected InitSeed command"),
        }

        let full = Args::try_parse_from([
            "ruskkserv",
            "init-seed",
            "--force",
            "--fasttext",
            "/tmp/cc.ja.300.vec",
            "--dict",
            "/tmp/SKK-JISYO.L",
            "--min-similarity",
            "0.62",
            "--min-margin",
            "0.06",
            "--max-per-target",
            "30",
            "--include-non-competing",
            "--output",
            "/tmp/out.json",
            "--send-reload",
        ])
        .unwrap();
        match full.command {
            Some(Command::InitSeed {
                force,
                fasttext,
                dict,
                min_similarity,
                min_margin,
                max_per_target,
                include_non_competing,
                output,
                send_reload,
            }) => {
                assert!(force);
                assert_eq!(
                    fasttext.as_deref(),
                    Some(std::path::Path::new("/tmp/cc.ja.300.vec"))
                );
                assert_eq!(
                    dict.as_deref(),
                    Some(std::path::Path::new("/tmp/SKK-JISYO.L"))
                );
                assert!((min_similarity - 0.62).abs() < 1e-6);
                assert!((min_margin - 0.06).abs() < 1e-6);
                assert_eq!(max_per_target, 30);
                assert!(include_non_competing);
                assert_eq!(output.as_deref(), Some(std::path::Path::new("/tmp/out.json")));
                assert!(send_reload);
            }
            _ => panic!("expected InitSeed command"),
        }
    }
}
