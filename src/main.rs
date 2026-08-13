use std::{
    env,
    error::Error,
    ffi::{OsStr, OsString},
    fmt, io,
    net::{Ipv4Addr, SocketAddr},
    path::PathBuf,
    process::{Command, ExitCode, Stdio},
    thread,
};

use mdfolio::{
    catalog::Catalog,
    server::{AppState, app},
};
use tokio::net::TcpListener;

const HELP: &str = "\
Read the Markdown already in your repositories

Usage: mdfolio [OPTIONS] [PATH]

Arguments:
  [PATH]  Directory to gather. Defaults to the current directory [default: .]

Options:
      --no-open      Keep the browser closed and print the local URL
      --port <PORT>  Loopback port. Zero chooses an available port [default: 0]
  -h, --help         Print help
  -V, --version      Print version
";

#[derive(Debug, Eq, PartialEq)]
struct Cli {
    path: PathBuf,
    no_open: bool,
    port: u16,
}

#[derive(Debug, Eq, PartialEq)]
enum CliAction {
    Run(Cli),
    Help,
    Version,
}

#[derive(Debug, Eq, PartialEq)]
struct CliError(String);

impl fmt::Display for CliError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl Error for CliError {}

#[tokio::main(flavor = "current_thread")]
async fn main() -> ExitCode {
    match parse_args(env::args_os().skip(1)) {
        Ok(CliAction::Help) => {
            print!("{HELP}");
            ExitCode::SUCCESS
        }
        Ok(CliAction::Version) => {
            println!("mdfolio {}", env!("CARGO_PKG_VERSION"));
            ExitCode::SUCCESS
        }
        Ok(CliAction::Run(cli)) => match run(cli).await {
            Ok(()) => ExitCode::SUCCESS,
            Err(error) => {
                eprintln!("error: {error}");
                ExitCode::FAILURE
            }
        },
        Err(error) => {
            eprintln!("error: {error}\n\nFor more information, try '--help'.");
            ExitCode::from(2)
        }
    }
}

fn parse_args<I, S>(args: I) -> Result<CliAction, CliError>
where
    I: IntoIterator<Item = S>,
    S: Into<OsString>,
{
    let mut args = args.into_iter().map(Into::into);
    let mut path = None;
    let mut no_open = false;
    let mut port = 0;
    let mut positional_only = false;

    while let Some(argument) = args.next() {
        if !positional_only {
            if argument == "--" {
                positional_only = true;
                continue;
            }
            if argument == "-h" || argument == "--help" {
                return Ok(CliAction::Help);
            }
            if argument == "-V" || argument == "--version" {
                return Ok(CliAction::Version);
            }
            if argument == "--no-open" {
                no_open = true;
                continue;
            }
            if argument == "--port" {
                let value = args
                    .next()
                    .ok_or_else(|| CliError("--port requires a value".to_owned()))?;
                port = parse_port(&value)?;
                continue;
            }
            if let Some(value) = argument
                .to_str()
                .and_then(|argument| argument.strip_prefix("--port="))
            {
                port = parse_port(OsStr::new(value))?;
                continue;
            }
            if argument.as_encoded_bytes().starts_with(b"-") {
                return Err(CliError(format!(
                    "unexpected argument '{}'",
                    argument.to_string_lossy()
                )));
            }
        }

        if path.replace(PathBuf::from(&argument)).is_some() {
            return Err(CliError(format!(
                "unexpected second path '{}'",
                argument.to_string_lossy()
            )));
        }
    }

    Ok(CliAction::Run(Cli {
        path: path.unwrap_or_else(|| PathBuf::from(".")),
        no_open,
        port,
    }))
}

fn parse_port(value: &OsStr) -> Result<u16, CliError> {
    let printable = value.to_string_lossy();
    let value = value
        .to_str()
        .ok_or_else(|| CliError(format!("invalid port '{printable}'")))?;
    value.parse().map_err(|_| {
        CliError(format!(
            "invalid port '{printable}' (expected 0 through 65535)"
        ))
    })
}

async fn run(cli: Cli) -> Result<(), Box<dyn Error>> {
    let catalog = Catalog::scan(&cli.path)?;
    print_catalog_summary(&catalog);

    let listener = TcpListener::bind(SocketAddr::from((Ipv4Addr::LOCALHOST, cli.port)))
        .await
        .map_err(|source| {
            io::Error::new(
                source.kind(),
                format!("binding 127.0.0.1:{} failed: {source}", cli.port),
            )
        })?;
    let address = listener.local_addr()?;
    let state = AppState::new(catalog);

    let url = format!("http://{address}/_mdfolio/");
    println!("{url}");

    if !cli.no_open {
        open_browser(url);
    }

    axum::serve(listener, app(state))
        .with_graceful_shutdown(shutdown_signal())
        .await
        .map_err(|source| {
            io::Error::new(source.kind(), format!("local server failed: {source}"))
        })?;

    Ok(())
}

fn open_browser(url: String) {
    let result = thread::Builder::new()
        .name("mdfolio-xdg-open".to_owned())
        .spawn(move || {
            match Command::new("xdg-open")
                .arg(&url)
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status()
            {
                Ok(status) if status.success() => {}
                Ok(status) => {
                    eprintln!("warning: the browser did not open: xdg-open exited with {status}");
                }
                Err(error) => eprintln!("warning: the browser did not open: {error}"),
            }
        });
    if let Err(error) = result {
        eprintln!("warning: the browser opener did not start: {error}");
    }
}

fn print_catalog_summary(catalog: &Catalog) {
    println!("mdfolio  {}", catalog.root().display());
    println!(
        "{} repositories · {} pages",
        catalog.repositories().len(),
        catalog.documents().len()
    );
    for diagnostic in catalog.diagnostics() {
        if let Some(path) = &diagnostic.path {
            eprintln!("warning: {}: {}", path.display(), diagnostic.message);
        } else {
            eprintln!("warning: {}", diagnostic.message);
        }
    }
}

async fn shutdown_signal() {
    if let Err(error) = tokio::signal::ctrl_c().await {
        eprintln!("warning: listening for Ctrl-C failed: {error}");
    }
}

#[cfg(test)]
mod tests {
    use std::{
        ffi::OsString,
        path::{Path, PathBuf},
    };

    use super::{Cli, CliAction, parse_args};

    fn parse(args: &[&str]) -> Result<CliAction, String> {
        parse_args(args.iter().copied()).map_err(|error| error.to_string())
    }

    #[test]
    fn defaults_to_the_current_directory_and_an_available_port() {
        assert_eq!(
            parse(&[]),
            Ok(CliAction::Run(Cli {
                path: PathBuf::from("."),
                no_open: false,
                port: 0,
            }))
        );
    }

    #[test]
    fn accepts_documented_flags_in_either_order_and_both_port_forms() {
        assert_eq!(
            parse(&["docs", "--no-open", "--port", "4040"]),
            Ok(CliAction::Run(Cli {
                path: PathBuf::from("docs"),
                no_open: true,
                port: 4040,
            }))
        );
        assert_eq!(
            parse(&["--port=0", "--no-open", "docs"]),
            Ok(CliAction::Run(Cli {
                path: PathBuf::from("docs"),
                no_open: true,
                port: 0,
            }))
        );
    }

    #[test]
    fn double_dash_allows_a_path_that_starts_with_a_dash() {
        assert_eq!(
            parse(&["--", "-folio"]),
            Ok(CliAction::Run(Cli {
                path: PathBuf::from("-folio"),
                no_open: false,
                port: 0,
            }))
        );
    }

    #[test]
    fn help_and_version_keep_short_and_long_forms() {
        assert_eq!(parse(&["-h"]), Ok(CliAction::Help));
        assert_eq!(parse(&["--help"]), Ok(CliAction::Help));
        assert_eq!(parse(&["-V"]), Ok(CliAction::Version));
        assert_eq!(parse(&["--version"]), Ok(CliAction::Version));
    }

    #[test]
    fn rejects_invalid_ports_unknown_flags_and_multiple_paths() {
        for args in [
            &["--port"][..],
            &["--port", "nope"],
            &["--port=-1"],
            &["--port=65536"],
            &["--no-open=true"],
            &["--unknown"],
            &["first", "second"],
        ] {
            assert!(parse(args).is_err(), "{args:?}");
        }
    }

    #[cfg(unix)]
    #[test]
    fn preserves_non_utf8_positional_paths() {
        use std::os::unix::ffi::OsStringExt;

        let path = OsString::from_vec(vec![b'f', 0x80]);
        let parsed = parse_args([path.clone()]).unwrap();

        assert_eq!(
            parsed,
            CliAction::Run(Cli {
                path: Path::new(&path).to_path_buf(),
                no_open: false,
                port: 0,
            })
        );
    }
}
