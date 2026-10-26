//! wyvrn-claude: rooms in which the people let in talk with Claude together. It is
//! a community server (community/1.0, so any Wyvrn app joins it, reads and posts
//! in its rooms as in a community's channels) whose every channel has a Claude
//! session answering in it (wyvrn-community's assistant.rs), with the operator's
//! Anthropic API key and the limits its settings give.

use wyvrn_community::{assistant::Assistant, config::Config, store::Role};
use wyvrn_service::now;

const USAGE: &str = "\
wyvrn-claude [--config <path>] [serve]              run the server
wyvrn-claude [--config <path>] add <did> [admin]    let someone in, or make them an admin
wyvrn-claude [--config <path>] usage                what has been asked of Claude

The config path is --config, else $CLAUDE_CONFIG, else config/default.toml. The API
key is ANTHROPIC_API_KEY (or [assistant] api_key). The rest is set as a community
server's is (COMMUNITY_NAME, COMMUNITY_ADMINS, ...: see wyvrn-community's README).";

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    let config_path = match args.iter().position(|arg| arg == "--config") {
        Some(at) if at + 1 < args.len() => {
            let path = args.remove(at + 1);
            args.remove(at);
            path
        }
        Some(_) => anyhow::bail!("--config needs a path\n\n{USAGE}"),
        None => std::env::var("CLAUDE_CONFIG").unwrap_or_else(|_| "config/default.toml".to_string()),
    };
    let mut config = Config::load(&config_path)?;
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    match args.as_slice() {
        [] | ["serve"] => {
            let mut assistant = config.assistant.take().unwrap_or_else(wyvrn_community::assistant::AssistantConfig::from_defaults);
            if let Ok(key) = std::env::var("ANTHROPIC_API_KEY") {
                if !key.trim().is_empty() {
                    assistant.api_key = key.trim().to_string();
                }
            }
            if let Ok(model) = std::env::var("CLAUDE_MODEL") {
                if !model.trim().is_empty() {
                    assistant.model = model.trim().to_string();
                }
            }
            let number = |name: &str| std::env::var(name).ok().and_then(|value| value.trim().parse::<u64>().ok());
            if let Some(daily) = number("CLAUDE_DAILY_REQUESTS") {
                assistant.daily_requests_per_member = daily as u32;
            }
            if let Some(tokens) = number("CLAUDE_PERIOD_TOKENS") {
                assistant.period_tokens = tokens;
            }
            if assistant.api_key.is_empty() {
                eprintln!("No ANTHROPIC_API_KEY: rooms will say Claude couldn't answer until one is set.");
            }
            wyvrn_community::serve::serve(config, Some(Assistant::new(assistant))).await
        }
        ["add", did] | ["add", did, "member"] => add(&config, did, Role::Member),
        ["add", did, "admin"] => add(&config, did, Role::Admin),
        ["usage"] => {
            let store = wyvrn_community::serve::open_store(&config)?;
            let at = now();
            let period = format!("assistant:period:{}", at / wyvrn_community::assistant::PERIOD_SECS);
            println!("tokens this period: {}", store.setting(&period)?.unwrap_or_else(|| "0".into()));
            Ok(())
        }
        ["help" | "--help" | "-h"] => {
            println!("{USAGE}");
            Ok(())
        }
        _ => anyhow::bail!("unrecognized arguments\n\n{USAGE}"),
    }
}

fn add(config: &Config, did: &str, role: Role) -> anyhow::Result<()> {
    if !did.starts_with("did:") {
        anyhow::bail!("{did} isn't a DID");
    }
    let store = wyvrn_community::serve::open_store(config)?;
    store.set_member(did, role, now())?;
    println!("let in as {}", role.as_str());
    Ok(())
}
