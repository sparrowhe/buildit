use anyhow::anyhow;
use buildit::{ensure_job_queue, Job, JobResult, WorkerHeartbeat, WorkerIdentifier};
use chrono::{DateTime, Local};
use clap::Parser;
use futures::StreamExt;
use jsonwebtoken::EncodingKey;
use lapin::{
    options::{BasicAckOptions, BasicConsumeOptions, BasicPublishOptions, QueueDeclareOptions},
    types::FieldTable,
    BasicProperties, ConnectionProperties,
};
use log::{error, info, warn};
use octocrab::models::pulls::PullRequest;
use once_cell::sync::Lazy;
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    path::PathBuf,
    sync::{Arc, Mutex},
    time::Duration,
};
use teloxide::{prelude::*, types::ParseMode, utils::command::BotCommands};

macro_rules! PR {
    () => {
        "Topic Description\n-----------------\n\n{}\n\nPackage(s) Affected\n-------------------\n\n{}\n\nSecurity Update?\n----------------\n\nNo\n\n\nBuild Order\n-----------\n\n\n```\n{}\n```\n\nTest Build(s) Done\n------------------\n\n**Primary Architectures**\n\n- [ ] AMD64 `amd64`   \n- [ ] AArch64 `arm64`\n \n<!-- - [ ] 32-bit Optional Environment `optenv32` -->\n<!-- - [ ] Architecture-independent `noarch` -->\n\n**Secondary Architectures**\n\n- [ ] Loongson 3 `loongson3`\n- [ ] MIPS R6 64-bit (Little Endian) `mips64r6el`\n- [ ] PowerPC 64-bit (Little Endian) `ppc64el`\n- [ ] RISC-V 64-bit `riscv64`"
    };
}

#[derive(BotCommands, Clone)]
#[command(
    rename_rule = "lowercase",
    description = "BuildIt! supports the following commands:"
)]
enum Command {
    #[command(description = "Display usage: /help")]
    Help,
    #[command(
        description = "Start a build job: /build [git-ref] [packages] [archs] (e.g., /build stable bash,fish amd64,arm64)"
    )]
    Build(String),
    #[command(description = "Start a build job from GitHub PR: /pr [pr-number]")]
    PR(String),
    #[command(description = "Show queue and server status: /status")]
    Status,
    #[command(
        description = "Open Pull Request by git-ref /openpr [title];[git-ref];[packages] (e.g., /openpr VSCode Survey 1.85.0;vscode-1.85.0;vscode,vscodium"
    )]
    OpenPR(String),
    #[command(description = "Login to github")]
    Login,
    #[command(description = "Start bot")]
    Start(String),
}

struct WorkerStatus {
    last_heartbeat: DateTime<Local>,
}

static WORKERS: Lazy<Arc<Mutex<BTreeMap<WorkerIdentifier, WorkerStatus>>>> =
    Lazy::new(|| Arc::new(Mutex::new(BTreeMap::new())));

async fn build_inner(
    git_ref: &str,
    packages: &Vec<String>,
    archs: &Vec<&str>,
    github_pr: Option<u64>,
    msg: &Message,
) -> anyhow::Result<()> {
    let conn = lapin::Connection::connect(&ARGS.amqp_addr, ConnectionProperties::default()).await?;

    let channel = conn.create_channel().await?;
    // for each arch, create a job
    for arch in archs {
        let job = Job {
            packages: packages.iter().map(|s| s.to_string()).collect(),
            git_ref: git_ref.to_string(),
            arch: arch.to_string(),
            tg_chatid: msg.chat.id,
            github_pr,
        };

        info!("Adding job to message queue {:?} ...", job);

        // each arch has its own queue
        let queue_name = format!("job-{}", job.arch);
        ensure_job_queue(&queue_name, &channel).await?;

        channel
            .basic_publish(
                "",
                &queue_name,
                BasicPublishOptions::default(),
                &serde_json::to_vec(&job)?,
                BasicProperties::default(),
            )
            .await?
            .await?;
    }
    Ok(())
}

async fn build(
    bot: &Bot,
    git_ref: &str,
    packages: &Vec<String>,
    archs: &Vec<&str>,
    github_pr: Option<u64>,
    msg: &Message,
) -> ResponseResult<()> {
    let mut archs = archs.clone();
    if archs.contains(&"mainline") {
        // follow https://github.com/AOSC-Dev/autobuild3/blob/master/sets/arch_groups/mainline
        archs.extend_from_slice(&[
            "amd64",
            "arm64",
            "loongarch64",
            "loongson3",
            "mips64r6el",
            "ppc64el",
            "riscv64",
        ]);
        archs.retain(|arch| *arch != "mainline");
    }
    archs.sort();
    archs.dedup();

    match build_inner(git_ref, &packages, &archs, github_pr, &msg).await {
        Ok(()) => {
            bot.send_message(
                            msg.chat.id,
                            format!(
                                "\n__*New Job Summary*__\n\n*Git reference*: {}\n{}*Architecture\\(s\\)*: {}\n*Package\\(s\\)*: {}\n",
                                teloxide::utils::markdown::escape(git_ref),
                                if let Some(pr) = github_pr { format!("*GitHub PR*: [\\#{}](https://github.com/AOSC-Dev/aosc-os-abbs/pull/{})\n", pr, pr) } else { String::new() },
                                archs.join(", "),
                                teloxide::utils::markdown::escape(&packages.join(", ")),
                            ),
                        ).parse_mode(ParseMode::MarkdownV2)
                        .await?;
        }
        Err(err) => {
            bot.send_message(msg.chat.id, format!("Failed to create job: {}", err))
                .await?;
        }
    }
    Ok(())
}

async fn status(args: &Args) -> anyhow::Result<String> {
    let mut res = String::from("__*Queue Status*__\n\n");
    let conn = lapin::Connection::connect(&ARGS.amqp_addr, ConnectionProperties::default()).await?;

    let channel = conn.create_channel().await?;
    for arch in [
        "amd64",
        "arm64",
        "loongarch64",
        "loongson3",
        "mips64r6el",
        "ppc64el",
        "riscv64",
    ] {
        let queue_name = format!("job-{}", arch);

        let queue = ensure_job_queue(&queue_name, &channel).await?;

        // read unacknowledged job count
        let mut unacknowledged_str = String::new();
        if let Some(api) = &args.rabbitmq_queue_api {
            let client = reqwest::Client::new();
            let res = client
                .get(format!("{}{}", api, queue_name))
                .send()
                .await?
                .json::<serde_json::Value>()
                .await?;
            if let Some(unacknowledged) = res
                .as_object()
                .and_then(|m| m.get("messages_unacknowledged"))
                .and_then(|v| v.as_i64())
            {
                unacknowledged_str = format!("{} job\\(s\\), ", unacknowledged);
            }
        }
        res += &format!(
            "*{}*: {}{} available server\\(s\\)\n",
            teloxide::utils::markdown::escape(&arch),
            unacknowledged_str,
            queue.consumer_count()
        );
    }

    res += "\n__*Server Status*__\n\n";
    let fmt = timeago::Formatter::new();
    if let Ok(lock) = WORKERS.lock() {
        for (identifier, status) in lock.iter() {
            res += &teloxide::utils::markdown::escape(&format!(
                "{} ({}): Online as of {}\n",
                identifier.hostname,
                identifier.arch,
                fmt.convert_chrono(status.last_heartbeat, Local::now())
            ));
        }
    }
    Ok(res)
}

async fn answer(bot: Bot, msg: Message, cmd: Command) -> ResponseResult<()> {
    match cmd {
        Command::Help => {
            bot.send_message(msg.chat.id, Command::descriptions().to_string())
                .await?;
        }
        Command::PR(arguments) => {
            if let Ok(pr_number) = str::parse::<u64>(&arguments) {
                match octocrab::instance()
                    .pulls("AOSC-Dev", "aosc-os-abbs")
                    .get(pr_number)
                    .await
                {
                    Ok(pr) => {
                        let git_ref = &pr.head.ref_field;
                        // find lines starting with #buildit
                        let packages: Vec<String> = pr
                            .body
                            .and_then(|body| {
                                body.lines()
                                    .filter(|line| line.starts_with("#buildit"))
                                    .map(|line| {
                                        line.split(" ")
                                            .map(str::to_string)
                                            .skip(1)
                                            .collect::<Vec<_>>()
                                    })
                                    .next()
                            })
                            .unwrap_or_else(Vec::new);
                        if packages.len() > 0 {
                            let archs = vec![
                                "amd64",
                                "arm64",
                                "loongson3",
                                "mips64r6el",
                                "ppc64el",
                                "riscv64",
                            ];
                            build(&bot, git_ref, &packages, &archs, Some(pr_number), &msg).await?;
                        } else {
                            bot.send_message(msg.chat.id, format!("Please list packages to build in pr info starting with '#buildit'."))
                                .await?;
                        }
                    }
                    Err(err) => {
                        bot.send_message(msg.chat.id, format!("Failed to get pr info: {err}."))
                            .await?;
                    }
                }
            } else {
                bot.send_message(
                    msg.chat.id,
                    format!("Got invalid pr description: {arguments}."),
                )
                .await?;
            }
        }
        Command::Build(arguments) => {
            let parts: Vec<&str> = arguments.split(" ").collect();
            if parts.len() == 3 {
                let git_ref = parts[0];
                let packages: Vec<String> = parts[1].split(",").map(str::to_string).collect();
                let archs: Vec<&str> = parts[2].split(",").collect();
                build(&bot, git_ref, &packages, &archs, None, &msg).await?;
                return Ok(());
            }

            bot.send_message(
                msg.chat.id,
                format!("Got invalid job description: {arguments}."),
            )
            .await?;
        }
        Command::Status => match status(&ARGS).await {
            Ok(status) => {
                bot.send_message(msg.chat.id, status)
                    .parse_mode(ParseMode::MarkdownV2)
                    .await?;
            }
            Err(err) => {
                bot.send_message(msg.chat.id, format!("Failed to get status: {}", err))
                    .await?;
            }
        },
        Command::OpenPR(arguments) => {
            let parts: Vec<&str> = arguments.split(";").collect();

            let secret = match ARGS.secret.as_ref() {
                Some(s) => s,
                None => {
                    bot.send_message(msg.chat.id, "SECRET is not set").await?;
                    return Ok(());
                }
            };

            let token = match get_token(&msg.chat.id, secret).await {
                Ok(s) => s.access_token,
                Err(e) => {
                    bot.send_message(msg.chat.id, format!("Got error: {e}"))
                        .await?;
                    return Ok(());
                }
            };

            if parts.len() == 3 {
                match open_pr(parts, token, secret, msg.chat.id).await {
                    Ok(url) => {
                        bot.send_message(msg.chat.id, format!("Successfully opened PR: {url}"))
                            .await?
                    }
                    Err(e) => {
                        bot.send_message(msg.chat.id, format!("Got error: {e}"))
                            .await?
                    }
                };

                return Ok(());
            }

            bot.send_message(
                msg.chat.id,
                format!("Got invalid job description: {arguments}."),
            )
            .await?;
        }
        Command::Login => {
            bot.send_message(msg.chat.id, "https://github.com/login/oauth/authorize?client_id=Iv1.bf26f3e9dd7883ae&redirect_uri=https://minzhengbu.aosc.io/login").await?;
        }
        Command::Start(arguments) => {
            if arguments.len() != 20 {
                return Ok(());
            } else {
                let client = reqwest::Client::new();
                let resp = client
                    .get(format!("https://minzhengbu.aosc.io/login_from_telegram"))
                    .query(&[
                        ("telegram_id", msg.chat.id.0.to_string()),
                        ("rid", arguments),
                    ])
                    .send()
                    .await
                    .and_then(|x| x.error_for_status());

                match resp {
                    Ok(_) => {
                        bot.send_message(msg.chat.id, "Successfully to login.")
                            .await?
                    }
                    Err(e) => {
                        bot.send_message(msg.chat.id, format!("Got error: {e}"))
                            .await?
                    }
                };
            }
        }
    };

    Ok(())
}

#[derive(Deserialize, Serialize, Debug)]
struct CallbackSecondLoginArgs {
    access_token: String,
    expires_in: i64,
    refresh_token: String,
    refresh_token_expires_in: i64,
    scope: String,
    token_type: String,
}

async fn get_token(msg_chatid: &ChatId, secret: &str) -> anyhow::Result<CallbackSecondLoginArgs> {
    let client = reqwest::Client::new();
    let resp = client
        .get("https://minzhengbu.aosc.io/get_token")
        .query(&[("id", &msg_chatid.0.to_string())])
        .header("secret", secret)
        .send()
        .await
        .and_then(|x| x.error_for_status())?;

    let token = resp.json().await?;

    Ok(token)
}

async fn open_pr(
    parts: Vec<&str>,
    access_token: String,
    secret: &str,
    msg_chatid: ChatId,
) -> anyhow::Result<String> {
    let id = ARGS
        .github_app_id
        .as_ref()
        .ok_or_else(|| anyhow!("GITHUB_APP_ID is not set"))?
        .parse::<u64>()?;

    let app_private_key = ARGS
        .github_app_key
        .as_ref()
        .ok_or_else(|| anyhow!("GITHUB_APP_KEY_PEM_PATH is not set"))?;

    let key = tokio::fs::read(app_private_key).await?;
    let key = tokio::task::spawn_blocking(move || jsonwebtoken::EncodingKey::from_rsa_pem(&key))
        .await??;

    let pr = open_pr_inner(access_token, &parts, id, key.clone()).await;

    match pr {
        Ok(pr) => Ok(pr.html_url.map(|x| x.to_string()).unwrap_or_else(|| pr.url)),
        Err(e) => match e {
            octocrab::Error::GitHub { source, .. }
                if source.message.contains("Bad credentials") =>
            {
                let client = reqwest::Client::new();
                client
                    .get("https://minzhengbu.aosc.io/refresh_token")
                    .header("secret", secret)
                    .query(&[("id", msg_chatid.0.to_string())])
                    .send()
                    .await
                    .and_then(|x| x.error_for_status())?;

                let token = get_token(&msg_chatid, secret).await?;
                let pr = open_pr_inner(token.access_token, &parts, id, key).await?;

                Ok(pr.html_url.map(|x| x.to_string()).unwrap_or_else(|| pr.url))
            }
            _ => return Err(e.into()),
        },
    }
}

async fn open_pr_inner(
    access_token: String,
    parts: &[&str],
    id: u64,
    key: EncodingKey,
) -> Result<PullRequest, octocrab::Error> {
    let crab = octocrab::Octocrab::builder()
        .app(id.into(), key)
        .user_access_token(access_token)
        .build()?;

    crab.pulls("AOSC-Dev", "aosc-os-abbs")
        .create(parts[0], parts[1], "stable")
        .draft(false)
        .maintainer_can_modify(true)
        .body(format!(PR!(), parts[2], parts[2], parts[2]))
        .send()
        .await
}

/// Observe job completion messages
pub async fn job_completion_worker_inner(bot: Bot, amqp_addr: &str) -> anyhow::Result<()> {
    let conn = lapin::Connection::connect(amqp_addr, ConnectionProperties::default()).await?;

    let channel = conn.create_channel().await?;
    let _queue = channel
        .queue_declare(
            "job-completion",
            QueueDeclareOptions {
                durable: true,
                ..QueueDeclareOptions::default()
            },
            FieldTable::default(),
        )
        .await?;

    let mut consumer = channel
        .basic_consume(
            "job-completion",
            "backend_server",
            BasicConsumeOptions::default(),
            FieldTable::default(),
        )
        .await?;

    while let Some(delivery) = consumer.next().await {
        let delivery = match delivery {
            Ok(delivery) => delivery,
            Err(err) => {
                error!("Got error in lapin delivery: {}", err);
                continue;
            }
        };

        if let Some(result) = serde_json::from_slice::<JobResult>(&delivery.data).ok() {
            info!("Processing job result {:?} ...", result);
            let success = result.successful_packages == result.job.packages;
            // Report job result to user
            bot.send_message(
                result.job.tg_chatid,
                format!(
                    "{} Job completed on {} \\({}\\)\n\n*Time elapsed*: {}\n{}{}*Architecture*: {}\n*Package\\(s\\) to build*: {}\n*Package\\(s\\) successfully built*: {}\n*Package\\(s\\) failed to build*: {}\n*Package\\(s\\) not built due to previous build failure*: {}\n\n[Build Log \\>\\>]({})\n",
                    if success { "✅️" } else { "❌" },
                    teloxide::utils::markdown::escape(&result.worker.hostname),
                    result.worker.arch,
                    teloxide::utils::markdown::escape(&format!("{:.2?}", result.elapsed)),
                    if let Some(git_commit) = &result.git_commit {
                        format!("*Git commit*: [{}](https://github.com/AOSC-Dev/aosc-os-abbs/commit/{})\n", &git_commit[..8], git_commit)
                    } else {
                        String::new()
                    },
                    if let Some(pr) = result.job.github_pr {
                        format!("*GitHub PR*: [\\#{}](https://github.com/AOSC-Dev/aosc-os-abbs/pull/{})\n", pr, pr)
                    } else {
                        String::new()
                    },
                    result.job.arch,
                    teloxide::utils::markdown::escape(&result.job.packages.join(", ")),
                    teloxide::utils::markdown::escape(&result.successful_packages.join(", ")),
                    teloxide::utils::markdown::escape(&result.failed_package.clone().unwrap_or(String::from("None"))),
                    teloxide::utils::markdown::escape(&result.skipped_packages.join(", ")),
                    result.log.clone().unwrap_or(String::from("None")),
                ),
            ).parse_mode(ParseMode::MarkdownV2)
            .await?;

            // if associated with github pr, update comments
            if let Some(github_access_token) = &ARGS.github_access_token {
                if let Some(pr) = result.job.github_pr {
                    let new_content = format!(
                        "{} Job completed on {} \\({}\\)\n\n**Time elapsed**: {}\n{}**Architecture**: {}\n**Package\\(s\\) to build**: {}\n**Package\\(s\\) successfully built**: {}\n**Package\\(s\\) failed to build**: {}\n\n[Build Log \\>\\>]({})\n",
                        if success { "✅️" } else { "❌" },
                        result.worker.hostname,
                        result.worker.arch,
                        format!("{:.2?}", result.elapsed),
                        if let Some(git_commit) = &result.git_commit {
                            format!("**Git commit**: [{}](https://github.com/AOSC-Dev/aosc-os-abbs/commit/{})\n", &git_commit[..8], git_commit)
                        } else {
                            String::new()
                        },
                        result.job.arch,
                        teloxide::utils::markdown::escape(&result.job.packages.join(", ")),
                        teloxide::utils::markdown::escape(&result.successful_packages.join(", ")),
                        teloxide::utils::markdown::escape(&result.failed_package.clone().unwrap_or(String::from("None"))),
                        result.log.unwrap_or(String::from("None")),
                    );

                    // update or create new comment
                    let page = octocrab::instance()
                        .issues("AOSC-Dev", "aosc-os-abbs")
                        .list_comments(pr)
                        .send()
                        .await?;

                    let crab = octocrab::Octocrab::builder()
                        .user_access_token(github_access_token.clone())
                        .build()?;

                    // TODO: handle paging
                    let mut found = false;
                    for comment in page {
                        // find existing comment generated by @aosc-buildit-bot
                        if comment.user.login == "aosc-buildit-bot" {
                            // found, append new data
                            found = true;
                            info!("Found existing comment, updating");

                            let mut body = String::new();
                            if let Some(orig) = &comment.body {
                                body += orig;
                                body += "\n";
                            }
                            body += &new_content;

                            crab.issues("AOSC-Dev", "aosc-os-abbs")
                                .update_comment(comment.id, body)
                                .await?;
                            break;
                        }
                    }

                    if !found {
                        info!("No existing comments, create one");
                        crab.issues("AOSC-Dev", "aosc-os-abbs")
                            .create_comment(pr, new_content)
                            .await?;
                    }
                }
            }
        }

        // finish
        if let Err(err) = delivery.ack(BasicAckOptions::default()).await {
            warn!(
                "Failed to delete job result {:?}, error: {:?}",
                delivery, err
            );
        } else {
            info!("Finished processing job result {:?}", delivery.delivery_tag);
        }
    }
    Ok(())
}

pub async fn job_completion_worker(bot: Bot, amqp_addr: String) -> anyhow::Result<()> {
    loop {
        info!("Starting job completion worker ...");
        if let Err(err) = job_completion_worker_inner(bot.clone(), &amqp_addr).await {
            error!("Got error while starting job completion worker: {}", err);
        }
        tokio::time::sleep(Duration::from_secs(5)).await;
    }
}

pub async fn heartbeat_worker_inner(amqp_addr: String) -> anyhow::Result<()> {
    let conn = lapin::Connection::connect(&amqp_addr, ConnectionProperties::default()).await?;

    let channel = conn.create_channel().await?;
    let queue_name = "worker-heartbeat";
    ensure_job_queue(&queue_name, &channel).await?;

    let mut consumer = channel
        .basic_consume(
            &queue_name,
            "worker-heartbeat",
            BasicConsumeOptions::default(),
            FieldTable::default(),
        )
        .await?;
    while let Some(delivery) = consumer.next().await {
        let delivery = match delivery {
            Ok(delivery) => delivery,
            Err(err) => {
                error!("Got error in lapin delivery: {}", err);
                continue;
            }
        };

        if let Some(heartbeat) = serde_json::from_slice::<WorkerHeartbeat>(&delivery.data).ok() {
            info!("Processing worker heartbeat {:?} ...", heartbeat);

            // update worker status
            if let Ok(mut lock) = WORKERS.lock() {
                if let Some(status) = lock.get_mut(&heartbeat.identifier) {
                    status.last_heartbeat = Local::now();
                } else {
                    lock.insert(
                        heartbeat.identifier.clone(),
                        WorkerStatus {
                            last_heartbeat: Local::now(),
                        },
                    );
                }
            }

            // finish
            if let Err(err) = delivery.ack(BasicAckOptions::default()).await {
                warn!("Failed to ack heartbeat {:?}, error: {:?}", delivery, err);
            } else {
                info!("Finished ack-ing heartbeat {:?}", delivery.delivery_tag);
            }
        }
    }

    Ok(())
}

pub async fn heartbeat_worker(amqp_addr: String) -> anyhow::Result<()> {
    loop {
        info!("Starting heartbeat worker ...");
        if let Err(err) = heartbeat_worker_inner(amqp_addr.clone()).await {
            error!("Got error while starting heartbeat worker: {}", err);
        }
        tokio::time::sleep(Duration::from_secs(5)).await;
    }
}

#[derive(Parser, Debug)]
#[command(author, version, about, long_about = None)]
struct Args {
    /// AMQP address to access message queue
    #[arg(env = "BUILDIT_AMQP_ADDR")]
    amqp_addr: String,

    /// RabbitMQ address to access queue api e.g. http://user:password@host:port/api/queues/vhost/
    #[arg(env = "BUILDIT_RABBITMQ_QUEUE_API")]
    rabbitmq_queue_api: Option<String>,

    /// GitHub access token
    #[arg(env = "BUILDIT_GITHUB_ACCESS_TOKEN")]
    github_access_token: Option<String>,

    /// Secret
    #[arg(env = "SECRET")]
    secret: Option<String>,

    #[arg(env = "GITHUB_APP_ID")]
    github_app_id: Option<String>,

    #[arg(env = "GITHUB_APP_KEY_PEM_PATH")]
    github_app_key: Option<PathBuf>,
}

static ARGS: Lazy<Args> = Lazy::new(|| Args::parse());

#[tokio::main]
async fn main() {
    dotenv::dotenv().ok();
    env_logger::init();

    info!("Starting AOSC BuildIt! server with args {:?}", *ARGS);

    let bot = Bot::from_env();

    tokio::spawn(heartbeat_worker(ARGS.amqp_addr.clone()));

    tokio::spawn(job_completion_worker(bot.clone(), ARGS.amqp_addr.clone()));

    Command::repl(bot, answer).await;
}
