use std::{
    io::{self, Write},
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
    pub fn parse() -> Result<Option<Self>, lexopt::Error> {
        #[derive(Debug, Default)]
        struct Partial {
            port: Option<u16>,
            path: Option<PathBuf>,
        }

        let mut args = Partial::default();

        let mut parser = Parser::from_env();
        let mut do_help = true;

        while let Some(arg) = parser.next()? {
            do_help = false;
            match arg {
                Arg::Value(arg) => match &mut args {
                    Partial { port: None, .. } => args.port = Some(arg.parse()?),
                    Partial {
                        port: Some(_),
                        path: None,
                    } => args.path = Some(PathBuf::from(arg)),
                    _ => return Err(Arg::Value(arg).unexpected()),
                },
                Arg::Short('h') | Arg::Long("help") => {
                    do_help = true;
                    break;
                }
                arg => return Err(arg.unexpected()),
            }
        }

        if do_help {
            match io::stdout().write_fmt(format_help!(
                app_name = parser.bin_name().unwrap_or(APP_NAME),
            )) {
                Ok(()) => return Ok(None),
                Err(e) => return Err(lexopt::Error::Custom(e.into())),
            }
        }

        Ok(Some(Args {
            port: args.port.ok_or("missing argument 'port'")?,
            path: args.path.unwrap_or_else(|| PathBuf::from(".")),
        }))
    }
}
