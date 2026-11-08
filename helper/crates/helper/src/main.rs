use clap::{Parser, Subcommand};
use scribe_common::{Error, Result, Settings};
use std::path::PathBuf;

#[derive(Parser)]
#[command(
    name = "scribe-helper",
    version,
    about = "Redline's local transcript helper"
)]
struct Cli {
    #[arg(long, global = true)]
    root: Option<PathBuf>,
    #[arg(long, global = true)]
    data_dir: Option<PathBuf>,
    #[command(subcommand)]
    command: Command,
}
#[derive(Subcommand)]
enum Command {
    Serve {
        #[arg(long, default_value_t = 8765)]
        port: u16,
    },
    NativeHost {
        origin: String,
    },
    Install {
        #[arg(long)]
        no_start: bool,
        #[arg(long)]
        home: Option<PathBuf>,
    },
    Uninstall {
        #[arg(long)]
        no_stop: bool,
        #[arg(long)]
        home: Option<PathBuf>,
    },
    Check,
    Start,
}
fn find_root(explicit: Option<PathBuf>) -> Result<PathBuf> {
    if let Some(root) = explicit {
        return std::fs::canonicalize(root).map_err(Into::into);
    }
    if let Some(root) = std::env::var_os("SCRIBE_ROOT") {
        return std::fs::canonicalize(root).map_err(Into::into);
    }
    let executable = std::env::current_exe()?;
    for ancestor in executable.ancestors().skip(1) {
        if ancestor.join("extension/manifest.json").is_file() {
            return Ok(ancestor.to_path_buf());
        }
    }
    let cwd = std::env::current_dir()?;
    if cwd.join("extension/manifest.json").is_file() {
        return Ok(cwd);
    }
    Err(Error::bad_request(
        "Specify --root with the folder holding the helper's data (a Redline checkout).",
    ))
}
fn home(value: Option<PathBuf>) -> Result<PathBuf> {
    value
        .or_else(|| std::env::var_os("HOME").map(PathBuf::from))
        .ok_or_else(|| Error::bad_request("The home folder could not be located."))
}

#[tokio::main]
async fn main() {
    let mut args: Vec<_> = std::env::args_os().collect();
    let native = args
        .first()
        .and_then(|a| std::path::Path::new(a).file_name())
        .is_some_and(|n| n == "scribe-native-host" || n == "native-host");
    if native {
        args.insert(1, "native-host".into());
    }
    let cli = Cli::parse_from(args);
    let result = run(cli).await;
    if let Err(error) = result {
        eprintln!("{}", error.message);
        std::process::exit(1);
    }
}
async fn run(cli: Cli) -> Result<()> {
    let root = find_root(cli.root)?;
    match cli.command {
        Command::Start => scribe_helper::native::ensure_helper_running().await,
        Command::Serve { port } => {
            scribe_helper::service::serve(Settings::load(&root, cli.data_dir)?, port).await
        }
        Command::NativeHost { origin } => {
            if !scribe_helper::native::run_host(&root, &origin).await? {
                std::process::exit(1);
            }
            Ok(())
        }
        Command::Install {
            no_start,
            home: location,
        } => {
            scribe_helper::install::install(
                &root,
                &home(location)?,
                &std::env::current_exe()?,
                !no_start,
            )
            .await?;
            println!(
                "The helper is installed. Load the extension folder in your browser, then open Transcripts settings to connect Azure."
            );
            Ok(())
        }
        Command::Uninstall {
            no_stop,
            home: location,
        } => {
            scribe_helper::install::uninstall(&root, &home(location)?, !no_stop).await?;
            println!(
                "Automatic startup and the browser bridge were removed. Your transcripts and Azure settings remain."
            );
            Ok(())
        }
        Command::Check => {
            let settings = Settings::load(&root, cli.data_dir)?;
            println!(
                "{}",
                serde_json::json!({"configured":settings.configured(),"dependencies":settings.dependencies(),"version":scribe_common::VERSION})
            );
            Ok(())
        }
    }
}
