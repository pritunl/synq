use crate::errors::Result;
use crate::config::Config;
use super::sender::DaemonSender;

pub async fn run(config: Config) -> Result<()> {
    DaemonSender::run(config).await
}
