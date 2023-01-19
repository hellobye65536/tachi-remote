use log::error;

mod args;
mod load;
mod server;

use args::Args;
use load::load_library;
use server::ServerBuilder;

fn main() {
    simple_logger::init_with_level(log::Level::Info).unwrap();

    if let Err(e) = try_main() {
        error!("error: {:#}", e);
        std::process::exit(1);
    }
}

fn try_main() -> anyhow::Result<()> {
    let Some(Args { port, path }) = Args::parse()? else { return Ok(()) };

    let lib = load_library(&[&path])?;

    ServerBuilder::new(port).run(lib)
}
