use std::io::IsTerminal;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use nodo_api::client::HelperClient;
use nodo_api::{Backend, DEFAULT_SOCKET};
use nodo_tui::app::{App, ScreenId};
use nodo_tui::ctl;
use nodo_tui::fixtures::fixture;
use nodo_tui::helper_backend::HelperBackend;
use nodo_tui::mock::{MockBackend, MOCK_PASSPHRASE};
use nodo_tui::runtime;
use nodo_tui::theme::Theme;

fn usage() -> String {
    let screens: Vec<&str> = ScreenId::ALL.iter().map(|id| id.slug()).collect();
    format!(
        "Usage: nodo-tui [--socket PATH]\n       nodo-tui --mock [--screen NAME] [--no-drive]\n       \
         nodo-tui [--socket PATH] --ctl COMMAND [ARGS]\n\n\
         Without --mock, nodo-tui talks to the nodo-helper service at {DEFAULT_SOCKET}.\n\n\
         --socket PATH   Use another helper socket.\n\
         --mock          Run against sample data instead of this device.\n\
         --screen NAME   With --mock, open a design screen directly, with its sample state.\n\
         --no-drive      With --mock, start with the data drive missing.\n\
         --ctl           Send one request and print the answer. --ctl help lists them.\n\n\
         The mock device's passphrase is \"{MOCK_PASSPHRASE}\".\n\
         Screens: {}\n",
        screens.join(", ")
    )
}

/// Prints the answer as data, for scripts. Exits nonzero on any error.
fn run_ctl(socket: &Path, args: &[String]) -> ExitCode {
    let request = match ctl::parse(args) {
        Ok(request) => request,
        Err(text) => {
            eprintln!("{text}");
            return ExitCode::from(2);
        }
    };
    let mut client = match HelperClient::connect(socket) {
        Ok(client) => client,
        Err(error) => {
            eprintln!("nodo-tui: cannot reach the helper at {}: {error}", socket.display());
            return ExitCode::FAILURE;
        }
    };
    let mut result = client.call(request);
    if let Ok(first) = &result {
        match ctl::follow_up(args, first) {
            Ok(Some(next)) => {
                println!("{first:#?}");
                result = client.call(next);
            }
            Ok(None) => {}
            Err(text) => {
                eprintln!("{text}");
                return ExitCode::from(2);
            }
        }
    }
    match result {
        Ok(response) => {
            println!("{response:#?}");
            ExitCode::SUCCESS
        }
        Err(error) => {
            println!("Err({error:?})");
            eprintln!("nodo-tui: {error}");
            ExitCode::FAILURE
        }
    }
}

fn main() -> ExitCode {
    let mut mock = false;
    let mut no_drive = false;
    let mut screen = None;
    let mut socket = PathBuf::from(DEFAULT_SOCKET);
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--mock" => mock = true,
            "--no-drive" => no_drive = true,
            "--screen" => {
                let slug = args.next();
                // The web flow v2 proposal: static mock screens, shown until a key is pressed.
                if let Some(scene) = slug.as_deref().and_then(nodo_tui::proposal::Scene::from_slug) {
                    let colorterm = std::env::var("COLORTERM").ok();
                    let theme = Theme::detect(colorterm.as_deref(), std::env::var_os("NO_COLOR").is_some());
                    return match nodo_tui::proposal::show(scene, theme) {
                        Ok(()) => ExitCode::SUCCESS,
                        Err(error) => {
                            eprintln!("nodo-tui: {error}");
                            ExitCode::FAILURE
                        }
                    };
                }
                let Some(id) = slug.as_deref().and_then(ScreenId::from_slug) else {
                    eprint!("Unknown screen.\n\n{}", usage());
                    return ExitCode::from(2);
                };
                screen = Some(id);
            }
            "--socket" => {
                let Some(path) = args.next() else {
                    eprint!("--socket needs a path.\n\n{}", usage());
                    return ExitCode::from(2);
                };
                socket = path.into();
            }
            "--ctl" => {
                let rest: Vec<String> = args.collect();
                return run_ctl(&socket, &rest);
            }
            "--help" | "-h" => {
                print!("{}", usage());
                return ExitCode::SUCCESS;
            }
            other => {
                eprint!("Unknown argument: {other}\n\n{}", usage());
                return ExitCode::from(2);
            }
        }
    }
    if !mock && (screen.is_some() || no_drive) {
        eprint!("--screen and --no-drive need --mock.\n\n{}", usage());
        return ExitCode::from(2);
    }
    if !std::io::stdin().is_terminal() || !std::io::stdout().is_terminal() {
        eprintln!("nodo-tui needs an interactive terminal.");
        return ExitCode::from(2);
    }

    let colorterm = std::env::var("COLORTERM").ok();
    let no_color = std::env::var_os("NO_COLOR").is_some();
    let theme = Theme::detect(colorterm.as_deref(), no_color).with_rounded(rounded_buttons());

    let result = if mock {
        match (screen, no_drive) {
            (Some(id), _) => {
                let (app, backend) = fixture(id);
                runtime::run(app, backend, theme)
            }
            (None, true) => runtime::run(App::new(), MockBackend::with_missing_drive(), theme),
            (None, false) => runtime::run(App::new(), MockBackend::new(), theme),
        }
    } else {
        let backend = match HelperBackend::connect(&socket) {
            Ok(backend) => backend,
            Err(error) => {
                eprintln!(
                    "nodo-tui: cannot reach the helper at {}: {error}\n\
                     Is the nodo-helper service running, and is this user root or in the nodo group?",
                    socket.display()
                );
                return ExitCode::FAILURE;
            }
        };
        runtime::run(App::new(), backend, theme)
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("nodo-tui: {error}");
            ExitCode::FAILURE
        }
    }
}

/// `[tui] rounded_buttons` from the user's config, then the system's, resolved for this terminal.
fn rounded_buttons() -> bool {
    use nodo_tui::theme::RoundedButtons;
    let mut paths = Vec::new();
    if let Some(home) = std::env::var_os("XDG_CONFIG_HOME").map(std::path::PathBuf::from).or_else(|| {
        std::env::var_os("HOME").map(|home| std::path::PathBuf::from(home).join(".config"))
    }) {
        paths.push(home.join("nodo/tui.toml"));
    }
    paths.push(std::path::PathBuf::from("/etc/nodo/tui.toml"));
    let choice = paths
        .iter()
        .find_map(|path| std::fs::read_to_string(path).ok())
        .map_or(RoundedButtons::Auto, |text| RoundedButtons::from_config(&text));
    let term = std::env::var("TERM").ok();
    let program = std::env::var("TERM_PROGRAM").ok();
    choice.resolve(term.as_deref(), program.as_deref(), std::env::var_os("KITTY_WINDOW_ID").is_some())
}
