# TachiRemote
A lightweight manga server designed to work with [Tachiyomi](https://tachiyomi.org) ([github](https://github.com/tachiyomiorg/tachiyomi)).

Disclaimer: This project is not associated with the Tachiyomi project in any way.

## Usage
Any folder in the directory tree containing an `info.toml` file is considered a manga.
Refer to the example [`info.toml`](example-info.toml).
```
tachi-remote 1.0.0

USAGE:
    tachi-remote [options] <port> [path]

ARGS:
    <port>          the port to listen on
    [path]          path to the library directory, defaults to the current working directory

OPTIONS:
    -h, --help      print help
```

## gen-manga
Automatically generates an info.toml using the current directory.
```
gen-manga 1.0.0
Prints a generated info.toml to stdout using the provided chapters.
Title will be the current directory name.

USAGE:
    gen-manga [options] [chapters...]

ARGS:
    [chapters...]              path to chapters

OPTIONS:
    -h, --help                 print help
    -c, --cover <path>         use path as cover instead of the first page of the first chapter
    -t, --titles <file>        use lines from file for chapter titles, can be passed multiple times
```

## Building
Requirements:
- Rust toolchain

Run
```
$ cargo build --release
```

After running, the executable should be found under `./target/release/`
