use clap::Parser;
use gpt_image_2_core::{Cli, Commands};

mod client;
mod daemon;
mod fanout;
mod jobs;

pub fn run(argv: &[String]) -> i32 {
    if is_daemon_command(argv) {
        return daemon::dispatch(argv);
    }
    if jobs::is_command(argv) {
        return jobs::dispatch(argv);
    }
    if fanout::is_command(argv) {
        return fanout::dispatch(argv);
    }
    if daemon::skip_daemon() {
        return gpt_image_2_core::run(argv);
    }
    match Cli::try_parse_from(argv) {
        Ok(cli) if matches!(cli.command, Commands::Images(_)) => client::run_images(&cli),
        _ => gpt_image_2_core::run(argv),
    }
}

fn is_daemon_command(argv: &[String]) -> bool {
    argv.iter()
        .skip(1)
        .filter(|arg| !arg.starts_with('-'))
        .next()
        .map(String::as_str)
        == Some("daemon")
}
