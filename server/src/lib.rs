use chrono::{DateTime, Local};
use clap::Parser;
use common::WorkerIdentifier;
use once_cell::sync::Lazy;
use std::{
    collections::BTreeMap,
    path::PathBuf,
    sync::{Arc, Mutex},
};

pub mod bot;
pub mod heartbeat;
pub mod job;
pub mod github;

pub struct WorkerStatus {
    pub last_heartbeat: DateTime<Local>,
}

pub static WORKERS: Lazy<Arc<Mutex<BTreeMap<WorkerIdentifier, WorkerStatus>>>> =
    Lazy::new(|| Arc::new(Mutex::new(BTreeMap::new())));

#[derive(Parser, Debug)]
#[command(author, version, about, long_about = None)]
pub struct Args {
    /// AMQP address to access message queue
    #[arg(env = "BUILDIT_AMQP_ADDR")]
    pub amqp_addr: String,

    /// RabbitMQ address to access queue api e.g. http://user:password@host:port/api/queues/vhost/
    #[arg(env = "BUILDIT_RABBITMQ_QUEUE_API")]
    pub rabbitmq_queue_api: Option<String>,

    /// GitHub access token
    #[arg(env = "BUILDIT_GITHUB_ACCESS_TOKEN")]
    pub github_access_token: Option<String>,

    /// Secret
    #[arg(env = "SECRET")]
    pub secret: Option<String>,

    #[arg(env = "GITHUB_APP_ID")]
    pub github_app_id: Option<String>,

    #[arg(env = "GITHUB_APP_KEY_PEM_PATH")]
    pub github_app_key: Option<PathBuf>,

    #[arg(env = "ABBS_PATH")]
    pub abbs_path: Option<PathBuf>,
}

pub static ARGS: Lazy<Args> = Lazy::new(|| Args::parse());
