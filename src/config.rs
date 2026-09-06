use std::net::SocketAddr;
use std::time::Duration;

use clap::Parser;

use crate::backend::{Backend, UpstreamEncoding};

#[derive(Debug, Parser)]
#[command(
    name = "ruskk",
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
    #[arg(long, default_value_t = 500)]
    pub yaskkserv2_timeout_ms: u64,
}

impl Args {
    pub fn primary(&self) -> Backend {
        Backend {
            name: "azoo-key-skkserv".to_owned(),
            addr: self.azookey,
            encoding: UpstreamEncoding::EucJpRequestUtf8Response,
            timeout: Duration::from_millis(self.azookey_timeout_ms),
        }
    }

    pub fn fallback(&self) -> Backend {
        Backend {
            name: "yaskkserv2".to_owned(),
            addr: self.yaskkserv2,
            encoding: UpstreamEncoding::EucJp,
            timeout: Duration::from_millis(self.yaskkserv2_timeout_ms),
        }
    }
}
