//! The command line itself: help, version, flag errors and exit codes.

mod common;

use common::assertrust;

#[test]
fn help_lists_the_commands() {
    let run = assertrust(&["--help"]);
    assert_eq!(run.code, 0);
    insta::assert_snapshot!(run.stdout);
}

#[test]
fn check_help_lists_its_flags() {
    let run = assertrust(&["check", "--help"]);
    assert_eq!(run.code, 0);
    insta::assert_snapshot!(run.stdout);
}

#[test]
fn version_prints_the_crate_version() {
    let run = assertrust(&["--version"]);
    assert_eq!(run.code, 0);
    assert_eq!(
        run.stdout.trim(),
        format!("assertrust {}", env!("CARGO_PKG_VERSION"))
    );
}

#[test]
fn only_one_mode_can_be_given() {
    let run = assertrust(&["check", "--range", "main..HEAD", "--staged"]);
    assert_eq!(run.code, 2, "a usage error is a tool error: {}", run.stderr);
    assert!(run.stderr.contains("cannot be used with"), "{}", run.stderr);
}

#[test]
fn unknown_format_is_a_usage_error() {
    let run = assertrust(&["check", "--format", "xml"]);
    assert_eq!(run.code, 2);
    assert!(run.stderr.contains("invalid value 'xml'"), "{}", run.stderr);
}

#[test]
fn missing_command_is_a_usage_error() {
    let run = assertrust(&[]);
    assert_eq!(run.code, 2);
}
