use std::ops::ControlFlow;

use log::error;

mod args;
mod load;
mod server;

use args::Args;
use load::load_library;
use server::ServerBuilder;

fn main() {
    simple_logger::init_with_level(log::Level::Info).unwrap();

    if let Err(e) = main_err() {
        error!("error: {:#}", e);
        std::process::exit(1);
    }
}

fn main_err() -> anyhow::Result<()> {
    let Args { port, path } = match Args::parse_args()? {
        ControlFlow::Continue(v) => v,
        ControlFlow::Break(()) => return Ok(()),
    };

    let lib = load_library(&[&path])?;

    ServerBuilder::new(port).run(lib)
}
