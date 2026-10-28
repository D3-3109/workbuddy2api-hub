use clap::Parser;
use std::{net::IpAddr, path::PathBuf};

#[derive(Parser, Clone)]
#[command(
    name = "wb2api",
    version,
    about = "Linux-first gateway; baseline 477ba36; selected upstream v1.8.0 fixes at 4f7ae6f"
)]
pub struct Args {
    #[arg(long, env = "HOST", default_value = "127.0.0.1")]
    pub host: IpAddr,
    #[arg(long, env = "PORT", default_value_t = 8788)]
    pub port: u16,
    #[arg(long)]
    pub lan: bool,
    #[arg(long, env = "API_KEY", hide_env_values = true)]
    pub api_key: Option<String>,
    #[arg(long, env = "WB_PANEL_PASSWORD", hide_env_values = true)]
    pub panel_password: Option<String>,
    #[arg(long, env = "ACCOUNTS_DIR", default_value = "accounts")]
    pub accounts_dir: PathBuf,
    #[arg(long, env = "WB_PROXY_USAGE_DIR", default_value = "usage")]
    pub usage_dir: PathBuf,
    #[arg(long, default_value = "You are a helpful assistant.")]
    pub system_prompt: String,
    #[arg(long, env = "WB_MAX_CONCURRENT_CHAT", default_value_t = 32)]
    pub max_concurrent_chat: usize,
    #[arg(long, env = "WB_CHAT_SLOT_WAIT", default_value_t = 30)]
    pub chat_slot_wait: u64,
    #[arg(long, env = "WB_MAX_PAYLOAD_BYTES", default_value_t = 16 * 1024 * 1024)]
    pub max_payload_bytes: usize,
    /// Use a loopback fake upstream for offline contract tests only.
    #[arg(long, hide = true)]
    pub test_upstream: Option<String>,
    /// Disable upstream background refreshes (useful for deterministic tests).
    #[arg(long)]
    pub offline: bool,
}

impl Args {
    pub fn validate(&mut self) -> anyhow::Result<()> {
        if self.lan && self.host.is_loopback() {
            self.host = "0.0.0.0".parse()?;
        }
        if self.max_concurrent_chat == 0 || self.max_concurrent_chat > 4096 {
            anyhow::bail!("--max-concurrent-chat must be between 1 and 4096");
        }
        if self.max_payload_bytes == 0 || self.max_payload_bytes > 256 * 1024 * 1024 {
            anyhow::bail!("--max-payload-bytes must be between 1 and 268435456");
        }
        if let Some(base) = &self.test_upstream {
            let u = url::Url::parse(base)?;
            let host: IpAddr = u
                .host_str()
                .unwrap_or("")
                .trim_matches(['[', ']'])
                .parse()
                .map_err(|_| {
                    anyhow::anyhow!("test upstream must use a numeric loopback address")
                })?;
            if u.scheme() != "http"
                || !host.is_loopback()
                || !self.host.is_loopback()
                || !u.username().is_empty()
                || u.password().is_some()
                || u.query().is_some()
                || u.fragment().is_some()
            {
                anyhow::bail!("test upstream requires HTTP loopback, loopback listener and no URL credentials");
            }
        }
        Ok(())
    }
}
