use std::sync::Arc;

use lapin::{Channel, ConnectionProperties};
use log::info;
use server::bot::Command;
use server::{bot::answer, heartbeat::heartbeat_worker, job::job_completion_worker, ARGS};
use teloxide::prelude::*;

#[tokio::main]
async fn main() {
    dotenv::dotenv().ok();
    env_logger::init();

    info!("Starting AOSC BuildIt! server with args {:?}", *ARGS);

    let bot = Bot::from_env();

    tokio::spawn(heartbeat_worker(ARGS.amqp_addr.clone()));
    tokio::spawn(job_completion_worker(bot.clone(), ARGS.amqp_addr.clone()));

    let conn = lapin::Connection::connect(&ARGS.amqp_addr, ConnectionProperties::default())
        .await
        .unwrap();

    let channel = Arc::new(conn.create_channel().await.unwrap());

    let handler =
        Update::filter_message().branch(dptree::entry().filter_command::<Command>().endpoint(
            |bot: Bot, channel: Arc<Channel>, msg: Message, cmd: Command| async move {
                answer(bot, msg, cmd, channel).await
            },
        ));

    Dispatcher::builder(bot, handler)
        // Pass the shared state to the handler as a dependency.
        .dependencies(dptree::deps![channel])
        .enable_ctrlc_handler()
        .build()
        .dispatch()
        .await;
}
