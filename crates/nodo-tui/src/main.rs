use std::io::IsTerminal;
use std::process::ExitCode;

use nodo_tui::app::{App, ScreenId};
use nodo_tui::fixtures::fixture;
use nodo_tui::mock::{MockBackend, MOCK_PASSPHRASE};
use nodo_tui::runtime;
use nodo_tui::theme::Theme;

fn usage() -> String {
    let screens: Vec<&str> = ScreenId::ALL.iter().map(|id| id.slug()).collect();
    format!(
        "Usage: nodo-tui --mock [--screen NAME] [--no-drive]\n\n\
         --mock          Run against sample data. This is the only working mode for now.\n\
         --screen NAME   Open a design screen directly, with its sample state.\n\
         --no-drive      Start with the data drive missing.\n\n\
         The mock device's passphrase is \"{MOCK_PASSPHRASE}\".\n\
         Screens: {}\n",
        screens.join(", ")
    )
}

fn main() -> ExitCode {
    let mut mock = false;
    let mut no_drive = false;
    let mut screen = None;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--mock" => mock = true,
            "--no-drive" => no_drive = true,
            "--screen" => {
                let Some(id) = args.next().as_deref().and_then(ScreenId::from_slug) else {
                    eprint!("Unknown screen.\n\n{}", usage());
                    return ExitCode::from(2);
                };
                screen = Some(id);
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
    if !mock {
        eprint!("A real backend is not available yet, so --mock is required.\n\n{}", usage());
        return ExitCode::from(2);
    }
    if !std::io::stdin().is_terminal() || !std::io::stdout().is_terminal() {
        eprintln!("nodo-tui needs an interactive terminal.");
        return ExitCode::from(2);
    }

    let colorterm = std::env::var("COLORTERM").ok();
    let no_color = std::env::var_os("NO_COLOR").is_some();
    let theme = Theme::detect(colorterm.as_deref(), no_color);

    let result = match (screen, no_drive) {
        (Some(id), _) => {
            let (app, backend) = fixture(id);
            runtime::run(app, backend, theme)
        }
        (None, true) => runtime::run(App::new(), MockBackend::with_missing_drive(), theme),
        (None, false) => runtime::run(App::new(), MockBackend::new(), theme),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("nodo-tui: {error}");
            ExitCode::FAILURE
        }
    }
}
