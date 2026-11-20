//! syki-cli: the lean command-line front-end for Syki Lite.
//!
//! Parsing is hand-rolled and the binary pulls in no argument-parsing crate,
//! so a cold start stays well under the 5 ms budget. Only the subcommands
//! listed in [`KEPT_SUBCOMMANDS`] are recognised; everything that was pruned
//! during the 2.0 slim-down is reported as unknown.

use std::process::exit;

use syki_core::rpc::RuntimeEnv;
use syki_core::APP_VERSION;

/// The only subcommands retained in the slim 2.0 command surface.
const KEPT_SUBCOMMANDS: &[&str] = &[
    "buffer", "tab", "ui", "pty", "ocr", "speech", "info", "scrap", "config",
];

/// Every pruned subcommand, kept for the help text and for regression tests.
const PRUNED_SUBCOMMANDS: &[&str] = &["jev", "slot", "lesson", "pack", "drop", "git"];

/// Outcome of parsing the command line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CliAction {
    /// Print the package version and exit successfully.
    Version,
    /// Print the top-level usage and exit successfully.
    Help,
    /// Print the usage of one kept subcommand and exit successfully.
    SubcommandHelp(&'static str),
    /// Print the instance report as JSON and exit successfully.
    Info,
    /// Kept subcommand whose implementation has not landed yet.
    Unimplemented(&'static str),
    /// Token that is not part of the kept command surface.
    Unknown(String),
}

/// Maps raw argv tokens (excluding the program name) to a [`CliAction`].
pub fn parse_cli_action<I, S>(args: I) -> CliAction
where
    I: IntoIterator<Item = S>,
    S: Into<String>,
{
    let argv: Vec<String> = args.into_iter().map(Into::into).collect();
    let Some(first) = argv.first().map(String::as_str) else {
        return CliAction::Help;
    };

    if matches!(first, "--version" | "-V") {
        return CliAction::Version;
    }
    if matches!(first, "--help" | "-h") {
        return CliAction::Help;
    }

    if let Some(kept) = KEPT_SUBCOMMANDS.iter().copied().find(|name| *name == first) {
        // `info` is a real command; the other kept names are placeholders
        // whose behaviour has not been ported yet.
        if second_requests_help(&argv) {
            return CliAction::SubcommandHelp(kept);
        }
        return if kept == "info" {
            CliAction::Info
        } else {
            CliAction::Unimplemented(kept)
        };
    }

    // Pruned subcommands (even with `--help`) and any other token.
    CliAction::Unknown(first.to_string())
}

/// True when the token after the subcommand asks for that subcommand's help.
fn second_requests_help(argv: &[String]) -> bool {
    argv.get(1).map(String::as_str) == Some("--help")
        || argv.get(1).map(String::as_str) == Some("-h")
}

/// Top-level usage block printed for `--help` / `Help`.
fn print_top_level_help() {
    println!("syki-cli {} - lean Syki Lite front-end", APP_VERSION);
    println!();
    println!("USAGE:");
    println!("    syki-cli [COMMAND] [ARGUMENTS...]");
    println!();
    println!("FLAGS:");
    println!("    -V, --version   Print the package version");
    println!("    -h, --help      Print this usage");
    println!();
    println!("COMMANDS:");
    for name in KEPT_SUBCOMMANDS {
        println!("    {:<8} {}", name, subcommand_summary(name));
    }
    println!();
    println!("PRUNED (no longer available): {}", PRUNED_SUBCOMMANDS.join(", "));
}

/// One-line description for each kept subcommand, used by both help levels.
fn subcommand_summary(name: &str) -> &'static str {
    match name {
        "buffer" => "Inspect and drive text buffers",
        "tab" => "Manage workspace tabs",
        "ui" => "Control the shell window",
        "pty" => "Run and observe pseudo-terminals",
        "ocr" => "Extract text from images",
        "speech" => "Speak and transcribe audio",
        "info" => "Print the instance report as JSON",
        "scrap" => "Fetch and clean web content",
        "config" => "Read or write configuration",
        _ => "General-purpose command",
    }
}

/// Usage block printed for `SubcommandHelp`.
fn print_subcommand_help(name: &str) {
    println!("syki-cli {} {}", APP_VERSION, name);
    println!("    {}", subcommand_summary(name));
    println!();
    println!("USAGE:");
    println!("    syki-cli {} [SUBCOMMAND] [ARGUMENTS...]", name);
    println!();
    println!("FLAGS:");
    println!("    -h, --help   Print this usage");
}

/// JSON instance report emitted by the `info` command.
fn print_info_report() {
    let env = RuntimeEnv::from_env();
    let report = serde_json::json!({
        "version": APP_VERSION,
        "dataDir": env.data_dir.to_string_lossy(),
        "instanceId": env.instance_id,
    });
    println!("{}", serde_json::to_string_pretty(&report).expect("report serialises"));
}

/// Reports an error on stderr and terminates with the usage-error code.
fn report_failure(message: &str) -> ! {
    eprint!("error: {}", message);
    eprintln!(" (run `syki-cli --help` for the command surface)");
    exit(2);
}

fn main() {
    let action = parse_cli_action(std::env::args().skip(1));
    match action {
        CliAction::Version => {
            println!("syki-cli {}", APP_VERSION);
            exit(0);
        }
        CliAction::Help => {
            print_top_level_help();
            exit(0);
        }
        CliAction::SubcommandHelp(name) => {
            print_subcommand_help(name);
            exit(0);
        }
        CliAction::Info => {
            print_info_report();
            exit(0);
        }
        CliAction::Unimplemented(name) => report_failure(&format!(
            "`{}` is part of the kept command surface but is not implemented yet",
            name
        )),
        CliAction::Unknown(token) => {
            report_failure(&format!("`{}` is not a Syki Lite command", token))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Builds the owned argument vector expected by the parser.
    fn argv(tokens: &[&str]) -> Vec<String> {
        tokens.iter().map(|t| (*t).to_string()).collect()
    }

    #[test]
    fn empty_arguments_show_top_level_help() {
        assert_eq!(parse_cli_action(argv(&[])), CliAction::Help);
    }

    #[test]
    fn version_and_help_flags_are_recognised() {
        assert_eq!(parse_cli_action(argv(&["--version"])), CliAction::Version);
        assert_eq!(parse_cli_action(argv(&["-V"])), CliAction::Version);
        assert_eq!(parse_cli_action(argv(&["--help"])), CliAction::Help);
        assert_eq!(parse_cli_action(argv(&["-h"])), CliAction::Help);
    }

    #[test]
    fn every_kept_subcommand_accepts_help() {
        for name in KEPT_SUBCOMMANDS {
            assert_eq!(
                parse_cli_action(argv(&[name, "--help"])),
                CliAction::SubcommandHelp(name),
                "`{}` should accept --help",
                name
            );
            assert_eq!(
                parse_cli_action(argv(&[name, "-h"])),
                CliAction::SubcommandHelp(name),
                "`{}` should accept -h",
                name
            );
        }
    }

    #[test]
    fn kept_subcommands_without_arguments_resolve_expected_actions() {
        assert_eq!(parse_cli_action(argv(&["info"])), CliAction::Info);
        assert_eq!(parse_cli_action(argv(&["info", "--json"])), CliAction::Info);
        for name in ["buffer", "tab", "ui", "pty", "ocr", "speech", "scrap", "config"] {
            assert_eq!(
                parse_cli_action(argv(&[name])),
                CliAction::Unimplemented(name),
                "`{}` should be reported as unimplemented",
                name
            );
        }
    }

    #[test]
    fn pruned_subcommands_are_rejected_even_with_help() {
        for name in PRUNED_SUBCOMMANDS {
            let expected = CliAction::Unknown((*name).to_string());
            assert_eq!(parse_cli_action(argv(&[name])), expected.clone());
            assert_eq!(
                parse_cli_action(argv(&[name, "--help"])),
                expected,
                "`{}` must not resurrect its help text",
                name
            );
        }
    }

    #[test]
    fn unrelated_tokens_are_unknown() {
        assert_eq!(
            parse_cli_action(argv(&["launch"])),
            CliAction::Unknown("launch".to_string())
        );
        assert_eq!(
            parse_cli_action(argv(&["--bogus", "--help"])),
            CliAction::Unknown("--bogus".to_string())
        );
    }
}
