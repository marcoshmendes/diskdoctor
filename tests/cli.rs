//! Integration tests of the binary: help in both languages, language selection, exit
//! codes and argument validation. They never touch drives (no privileges needed).

use assert_cmd::Command;
use predicates::prelude::*;
use tempfile::TempDir;

/// The binary with an isolated data folder, so a language saved on this machine (or in
/// the environment) never changes the results.
fn bin() -> (Command, TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let mut cmd = Command::cargo_bin("diskdoctor").expect("binary built");
    cmd.env("DISKDOCTOR_DATA_DIR", dir.path()).env_remove("DISKDOCTOR_LANG").env("NO_COLOR", "1");
    (cmd, dir)
}

#[test]
fn help_is_in_english_by_default() {
    let (mut cmd, _dir) = bin();
    cmd.arg("--help")
        .assert()
        .success()
        .stdout(predicate::str::contains("Usage:"))
        .stdout(predicate::str::contains("Commands"))
        .stdout(predicate::str::contains("Non-destructive diagnostics"))
        .stdout(predicate::str::contains("diskdoctor scan --disk 1 --read-only"))
        .stdout(predicate::str::contains("--lang"));
}

#[test]
fn help_in_portuguese_keeps_technical_names() {
    let (mut cmd, _dir) = bin();
    cmd.args(["--lang", "pt-BR", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Uso:"))
        .stdout(predicate::str::contains("Comandos"))
        .stdout(predicate::str::contains("Diagnóstico não destrutivo"))
        .stdout(predicate::str::contains("Surface Scan"))
        .stdout(predicate::str::contains("Capacity Verification"));
}

#[test]
fn environment_variable_selects_the_language() {
    let (mut cmd, _dir) = bin();
    cmd.env("DISKDOCTOR_LANG", "pt-BR").args(["scan", "--help"]).assert().success().stdout(predicate::str::contains("nunca grava"));
    let (mut cmd, _dir) = bin();
    cmd.env("DISKDOCTOR_LANG", "pt-BR").args(["--lang", "en", "scan", "--help"]).assert().success().stdout(predicate::str::contains("never writes"));
}

#[test]
fn language_command_saves_the_preference() {
    let dir = tempfile::tempdir().unwrap();
    let run = |args: &[&str]| {
        let mut cmd = Command::cargo_bin("diskdoctor").unwrap();
        cmd.env("DISKDOCTOR_DATA_DIR", dir.path()).env_remove("DISKDOCTOR_LANG").env("NO_COLOR", "1").args(args).assert()
    };
    run(&["language"]).success().stdout(predicate::str::contains("Interface language: English (default)"));
    run(&["language", "pt-BR"]).success().stdout(predicate::str::contains("Idioma definido: Português (Brasil)"));
    let saved = std::fs::read_to_string(dir.path().join("settings.json")).unwrap();
    assert!(saved.contains("\"pt-BR\""), "{saved}");
    run(&["--help"]).success().stdout(predicate::str::contains("Uso:"));
    run(&["language"]).success().stdout(predicate::str::contains("preferência salva"));
    run(&["lang", "en", "--json"]).success().stdout(predicate::str::contains("\"language\": \"en\""));
    run(&["--help"]).success().stdout(predicate::str::contains("Usage:"));
}

#[test]
fn invalid_language_is_a_usage_error() {
    let (mut cmd, _dir) = bin();
    cmd.args(["list", "--lang", "fr"]).assert().code(2).stderr(predicate::str::contains("unknown language"));
    let (mut cmd, dir) = bin();
    cmd.args(["language", "xx"]).assert().code(2);
    assert!(!dir.path().join("settings.json").exists());
}

#[test]
fn subcommand_help_mentions_read_only_guarantee() {
    let (mut cmd, _dir) = bin();
    cmd.args(["scan", "--help"]).assert().success().stdout(predicate::str::contains("never writes")).stdout(predicate::str::contains("--timeout-ms"));
}

#[test]
fn version_flag() {
    let (mut cmd, _dir) = bin();
    cmd.arg("--version").assert().success().stdout(predicate::str::contains(env!("CARGO_PKG_VERSION")));
}

#[test]
fn unknown_argument_is_usage_error_in_both_languages() {
    let (mut cmd, _dir) = bin();
    cmd.args(["list", "--does-not-exist"])
        .assert()
        .code(2)
        .stderr(predicate::str::contains("error:"))
        .stderr(predicate::str::contains("unexpected argument"));
    let (mut cmd, _dir) = bin();
    cmd.args(["--lang", "pt-BR", "list", "--nao-existe"])
        .assert()
        .code(2)
        .stderr(predicate::str::contains("erro:"))
        .stderr(predicate::str::contains("argumento inesperado"));
}

#[test]
fn write_test_cannot_be_combined_with_read_only() {
    let (mut cmd, _dir) = bin();
    cmd.args(["benchmark", "--disk", "1", "--read-only", "--write-test", "D:\\"])
        .assert()
        .code(2)
        .stderr(predicate::str::contains("cannot be used with"));
}

#[test]
fn scan_requires_disk_number() {
    let (mut cmd, _dir) = bin();
    cmd.arg("scan").assert().code(2).stderr(predicate::str::contains("required"));
}

#[test]
fn no_command_outside_a_terminal_prints_help() {
    // Under assert_cmd there is no terminal: instead of the interactive menu, help is shown.
    let (mut cmd, _dir) = bin();
    cmd.assert().code(2).stdout(predicate::str::contains("interactive menu"));
    let (mut cmd, _dir) = bin();
    cmd.args(["--lang", "pt-BR"]).assert().code(2).stdout(predicate::str::contains("menu interativo"));
}

#[test]
fn capacity_help_explains_what_is_written() {
    let (mut cmd, _dir) = bin();
    cmd.args(["capacity", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("NEW files"))
        .stdout(predicate::str::contains("type WRITE"))
        .stdout(predicate::str::contains("system drive is refused"));
    let (mut cmd, _dir) = bin();
    cmd.args(["--lang", "pt-BR", "capacity", "--help"]).assert().success().stdout(predicate::str::contains("digitar WRITE"));
}

#[test]
fn capacity_refuses_json_mode() {
    let (mut cmd, _dir) = bin();
    cmd.args(["capacity", "--volume", "Z:", "--json"]).assert().code(1).stderr(predicate::str::contains("typed confirmation"));
}

#[test]
fn tag_and_history_help_show_examples() {
    let (mut cmd, _dir) = bin();
    cmd.args(["tag", "--help"]).assert().success().stdout(predicate::str::contains("diskdoctor tag add --disk 2 Studio"));
    let (mut cmd, _dir) = bin();
    cmd.args(["history", "--help"]).assert().success().stdout(predicate::str::contains("Replaced the SATA cable"));
}

#[test]
fn numeric_only_tags_are_rejected_before_touching_anything() {
    let (mut cmd, dir) = bin();
    cmd.args(["tag", "add", "--disk", "0", "123"]).assert().code(1).stderr(predicate::str::contains("cannot be digits only"));
    assert!(!dir.path().join("tags.json").exists());
}
