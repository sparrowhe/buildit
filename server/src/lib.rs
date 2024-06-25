use axum::{extract::connect_info, serve::IncomingStream};
use clap::Parser;
use diesel::{
    r2d2::{ConnectionManager, Pool},
    PgConnection,
};
use once_cell::sync::Lazy;
use std::{net::SocketAddr, path::PathBuf, sync::Arc};
use tokio::net::{unix::UCred, UnixStream};

pub mod api;
pub mod bot;
pub mod formatter;
pub mod github;
pub mod models;
pub mod recycler;
pub mod routes;
pub mod schema;

pub type DbPool = Pool<ConnectionManager<PgConnection>>;

#[derive(Parser, Debug)]
#[command(author, version, about, long_about = None)]
pub struct Args {
    /// Database connection url
    #[arg(env = "DATABASE_URL")]
    pub database_url: String,

    #[arg(env = "BUILDIT_ABBS_PATH")]
    pub abbs_path: PathBuf,

    /// GitHub access token
    #[arg(env = "BUILDIT_GITHUB_ACCESS_TOKEN")]
    pub github_access_token: String,

    #[arg(env = "BUILDIT_WORKER_SECRET")]
    pub worker_secret: String,

    /// Secret
    #[arg(env = "BUILDIT_GITHUB_SECRET")]
    pub github_secret: Option<String>,

    #[arg(env = "BUILDIT_GITHUB_APP_ID")]
    pub github_app_id: Option<String>,

    #[arg(env = "BUILDIT_GITHUB_APP_KEY_PEM_PATH")]
    pub github_app_key: Option<PathBuf>,

    /// Development mode
    #[arg(env = "BUILDIT_DEVELOPMENT")]
    pub development_mode: Option<bool>,

    /// OpenTelemetry
    #[arg(env = "BUILDIT_OTLP")]
    pub otlp_url: Option<String>,

    /// Local repo path if available
    #[arg(env = "BUILDIT_REPO_PATH")]
    pub local_repo: Option<PathBuf>,

    /// Listen to unix socket if set
    #[arg(env = "BUILDIT_LISTEN_SOCKET_PATH")]
    pub unix_socket: Option<PathBuf>,
}

pub static ARGS: Lazy<Args> = Lazy::new(Args::parse);
pub const HEARTBEAT_TIMEOUT: i64 = 600; // 10 minutes

// follow https://github.com/AOSC-Dev/autobuild3/blob/master/sets/arch_groups/mainline
pub(crate) const ALL_ARCH: &[&str] = &[
    "amd64",
    "arm64",
    "loongarch64",
    "loongson3",
    "mips64r6el",
    "ppc64el",
    "riscv64",
];

// https://github.com/tokio-rs/axum/blob/main/examples/unix-domain-socket/src/main.rs
#[derive(Clone, Debug)]
pub enum RemoteAddr {
    Uds(UdsSocketAddr),
    Inet(SocketAddr),
}

#[derive(Clone, Debug)]
#[allow(dead_code)]
pub struct UdsSocketAddr {
    peer_addr: Arc<tokio::net::unix::SocketAddr>,
    peer_cred: UCred,
}

impl connect_info::Connected<&UnixStream> for RemoteAddr {
    fn connect_info(target: &UnixStream) -> Self {
        let peer_addr = target.peer_addr().unwrap();
        let peer_cred = target.peer_cred().unwrap();

        Self::Uds(UdsSocketAddr {
            peer_addr: Arc::new(peer_addr),
            peer_cred,
        })
    }
}

impl<'a> connect_info::Connected<IncomingStream<'a>> for RemoteAddr {
    fn connect_info(target: IncomingStream) -> Self {
        Self::Inet(target.remote_addr())
    }
}
