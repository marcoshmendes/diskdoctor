//! Testes de integração do binário: ajuda em pt-BR, códigos de saída e validação
//! de argumentos. Não tocam em discos (não dependem de privilégios).

use assert_cmd::Command;
use predicates::prelude::*;

fn bin() -> Command {
    Command::cargo_bin("diskdoctor").expect("binário compilado")
}

#[test]
fn help_is_in_portuguese() {
    bin()
        .arg("--help")
        .assert()
        .success()
        .stdout(predicate::str::contains("Uso:"))
        .stdout(predicate::str::contains("Comandos"))
        .stdout(predicate::str::contains("Diagnóstico não destrutivo"))
        .stdout(predicate::str::contains("diskdoctor scan --disk 1 --read-only"));
}

#[test]
fn subcommand_help_mentions_read_only_guarantee() {
    bin()
        .args(["scan", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("nunca grava"))
        .stdout(predicate::str::contains("--timeout-ms"));
}

#[test]
fn version_flag() {
    bin().arg("--version").assert().success().stdout(predicate::str::contains(env!("CARGO_PKG_VERSION")));
}

#[test]
fn unknown_argument_is_usage_error_in_portuguese() {
    bin()
        .args(["list", "--nao-existe"])
        .assert()
        .code(2)
        .stderr(predicate::str::contains("erro:"))
        .stderr(predicate::str::contains("argumento inesperado"));
}

#[test]
fn write_test_cannot_be_combined_with_read_only() {
    bin()
        .args(["benchmark", "--disk", "1", "--read-only", "--write-test", "D:\\"])
        .assert()
        .code(2)
        .stderr(predicate::str::contains("não pode ser usado com"));
}

#[test]
fn scan_requires_disk_number() {
    bin().arg("scan").assert().code(2).stderr(predicate::str::contains("obrigatórios"));
}

#[test]
fn no_command_outside_a_terminal_prints_help() {
    // Sob o assert_cmd não há terminal: em vez do menu interativo, mostra a ajuda.
    bin().assert().code(2).stdout(predicate::str::contains("menu interativo"));
}

#[test]
fn capacity_help_explains_what_is_written() {
    bin()
        .args(["capacity", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("arquivos NOVOS"))
        .stdout(predicate::str::contains("GRAVAR"))
        .stdout(predicate::str::contains("disco do sistema é recusado"));
}

#[test]
fn capacity_refuses_json_mode() {
    bin().args(["capacity", "--volume", "Z:", "--json"]).assert().code(1).stderr(predicate::str::contains("confirmação digitada"));
}

#[test]
fn tag_and_history_help_show_examples() {
    bin().args(["tag", "--help"]).assert().success().stdout(predicate::str::contains("diskdoctor tag add --disk 2 Studio"));
    bin().args(["history", "--help"]).assert().success().stdout(predicate::str::contains("Troquei o cabo SATA"));
}

#[test]
fn numeric_only_tags_are_rejected_before_touching_anything() {
    let dir = tempfile::tempdir().unwrap();
    bin()
        .env("DISKDOCTOR_DATA_DIR", dir.path())
        .args(["tag", "add", "--disk", "0", "123"])
        .assert()
        .code(1)
        .stderr(predicate::str::contains("não pode ser só números"));
    assert!(!dir.path().join("tags.json").exists());
}
