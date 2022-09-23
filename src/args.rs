use std::{
    io::{self, Write},
    ops::ControlFlow,
    path::PathBuf,
};

use lexopt::{Arg, Parser, ValueExt};

#[rustfmt::skip]
const HELP: &str = concat!(
    "tachi-remote ", env!("CARGO_PKG_VERSION"), "\n",
    "\n",
    "USAGE:\n",
    "    tachi-remote [options] <port> [path]\n",
    "\n",
    "ARGS:\n",
    "    <port>        the port to listen on\n",
    "    [path]        path to the library directory, default is the current working directory\n",
    "\n",
    "OPTIONS:\n",
    "    -h, --help    print help\n",
);

fn print_help() {
    io::stdout().write_all(HELP.as_bytes()).unwrap();
}

#[derive(Debug)]
pub struct Args {
    pub port: u16,
    pub path: PathBuf,
}

impl TryFrom<ArgsPartial> for Args {
    type Error = anyhow::Error;

    fn try_from(v: ArgsPartial) -> Result<Self, Self::Error> {
        Ok(Self {
            port: v.port.ok_or_else(|| anyhow::anyhow!("missing port"))?,
            path: v.path.unwrap_or_else(|| PathBuf::from(".")),
        })
    }
}

#[derive(Debug, Default)]
struct ArgsPartial {
    port: Option<u16>,
    path: Option<PathBuf>,
}

impl Args {
    pub fn parse_args() -> anyhow::Result<ControlFlow<(), Args>> {
        let mut args = ArgsPartial::default();
        let mut arg_index = 0usize;

        let mut parser = Parser::from_env();
        let mut any_args = false;

        while let Some(arg) = parser.next()? {
            any_args = true;
            match arg {
                Arg::Value(v) => {
                    match arg_index {
                        0 => args.port = Some(v.parse()?),
                        1 => args.path = Some(PathBuf::from(v)),
                        _ => Err(Arg::Value(v).unexpected())?,
                    }
                    arg_index += 1;
                }
                Arg::Short('h') | Arg::Long("help") => {
                    print_help();
                    return Ok(ControlFlow::Break(()));
                }
                arg => Err(arg.unexpected())?,
            }
        }

        if !any_args {
            print_help();
            return Ok(ControlFlow::Break(()));
        }

        Ok(ControlFlow::Continue(args.try_into()?))
    }
}
