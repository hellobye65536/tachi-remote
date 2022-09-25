use std::{
    io::{self, Write},
    ops::ControlFlow,
    path::PathBuf,
};

use lexopt::{Arg, Parser, ValueExt};

const APP_NAME: &str = "tachi-remote";

macro_rules! format_help {
    ($($v:tt)*) => {
        format_args!(
            concat!(
                "{app_name} ", env!("CARGO_PKG_VERSION"), "\n",
                "\n",
                "USAGE:\n",
                "    {app_name} [options] <port> [path]\n",
                "\n",
                "ARGS:\n",
                "    <port>          the port to listen on\n",
                "    [path]          path to the library directory, defaults to the current working directory\n",
                "\n",
                "OPTIONS:\n",
                "    -h, --help      print help\n",
            ),
            $($v)*
        )
    };
}

#[derive(Debug)]
pub struct Args {
    pub port: u16,
    pub path: PathBuf,
}

impl Args {
    pub fn parse_args() -> Result<ControlFlow<(), Self>, lexopt::Error> {
        #[derive(Debug, Default)]
        struct ArgsPartial {
            port: Option<u16>,
            path: Option<PathBuf>,
        }

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
                        _ => return Err(Arg::Value(v).unexpected()),
                    }
                    arg_index += 1;
                }
                Arg::Short('h') | Arg::Long("help") => {
                    any_args = false;
                    break;
                }
                arg => return Err(arg.unexpected()),
            }
        }

        if !any_args {
            match io::stdout().write_fmt(format_help!(
                app_name = parser.bin_name().unwrap_or(APP_NAME),
            )) {
                Ok(()) => return Ok(ControlFlow::Break(())),
                Err(e) => return Err(lexopt::Error::Custom(e.into())),
            }
        }

        Ok(ControlFlow::Continue(Args {
            port: args.port.ok_or("missing argument 'port'")?,
            path: args.path.unwrap_or_else(|| PathBuf::from(".")),
        }))
    }
}
