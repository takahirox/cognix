//! Shared argument contract for Cognix and the development launcher helper.
use clap::Parser;
use std::path::PathBuf;

fn positive(s: &str) -> Result<usize, String> {
    s.parse::<usize>()
        .ok()
        .filter(|n| *n > 0)
        .ok_or("must be greater than zero".into())
}
fn seconds(s: &str) -> Result<usize, String> {
    positive(s).and_then(|n| {
        if n <= 86400 {
            Ok(n)
        } else {
            Err("must be at most 86400 seconds".into())
        }
    })
}
#[derive(Parser)]
#[command(
    version,
    about = "Cognix: bounded local edit-and-verify agent. Ctrl-C cancels; /exit quits."
)]
pub struct Args {
    #[arg(long, default_value = ".")]
    pub root: PathBuf,
    #[arg(long, env = "COGNIX_MODEL", default_value = "qwen3-coder:30b")]
    pub model: String,
    #[arg(
        long,
        env = "COGNIX_OLLAMA_URL",
        default_value = "http://127.0.0.1:11434"
    )]
    pub ollama_url: String,
    #[arg(long)]
    pub task: Option<String>,
    #[arg(long, value_name = "JSON_ARGV")]
    pub allow_command: Vec<String>,
    #[arg(long,default_value="16",value_parser=positive)]
    pub max_steps: usize,
    #[arg(long,default_value="16384",value_parser=positive)]
    pub num_ctx: usize,
    #[arg(long,default_value="120",value_parser=seconds)]
    pub model_timeout: usize,
    #[arg(long,default_value="30",value_parser=seconds)]
    pub command_timeout: usize,
    #[arg(long,default_value="16384",value_parser=positive)]
    pub output_limit: usize,
}
