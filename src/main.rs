#![allow(clippy::too_many_arguments, clippy::type_complexity)]

mod arena;
mod auto;
mod bench;
mod cli;
mod command;
mod dispatch;
mod engine;
mod error;
mod fleet;
mod graph;
mod hooks;
mod kb;
mod peers;
mod settings;
mod store;
mod sweep;
mod tmux;
mod util;
mod view;

fn main() {
    let code = cli::run();
    std::process::exit(code);
}
