//! Execução de scripts PowerShell que devolvem JSON.
//!
//! Usamos o Windows PowerShell 5.1 (`powershell.exe`), presente em todo Windows
//! 10/11, em vez de exigir o PowerShell 7. O script vai via `-EncodedCommand`
//! (UTF-16LE em base64) para evitar qualquer problema de aspas/escape, e a saída
//! é forçada para UTF-8 para preservar acentos das mensagens de evento em pt-BR.

use super::process::run_with_timeout;
use anyhow::{Result, bail};
use base64::Engine;
use regex::Regex;
use std::path::PathBuf;
use std::sync::LazyLock;
use std::time::Duration;

const PREAMBLE: &str = "$ErrorActionPreference = 'Stop'\n\
$ProgressPreference = 'SilentlyContinue'\n\
[Console]::OutputEncoding = [System.Text.Encoding]::UTF8\n";

/// Codifica o script (com o preâmbulo) para `-EncodedCommand`.
pub fn encode_command(script: &str) -> String {
    let full = format!("{PREAMBLE}{script}");
    let bytes: Vec<u8> = full.encode_utf16().flat_map(u16::to_le_bytes).collect();
    base64::engine::general_purpose::STANDARD.encode(bytes)
}

fn powershell_exe() -> PathBuf {
    if let Some(root) = std::env::var_os("SystemRoot") {
        let p = PathBuf::from(root).join(r"System32\WindowsPowerShell\v1.0\powershell.exe");
        if p.exists() {
            return p;
        }
    }
    PathBuf::from("powershell.exe")
}

/// Executa o script e devolve o stdout (esperado: JSON).
pub fn run_script(script: &str, timeout: Duration) -> Result<String> {
    let encoded = encode_command(script);
    let args = ["-NoProfile", "-NonInteractive", "-ExecutionPolicy", "Bypass", "-EncodedCommand", encoded.as_str()];
    let out = run_with_timeout(powershell_exe(), &args, timeout)?;
    if out.code != Some(0) {
        bail!("o PowerShell retornou erro: {}", clean_stderr(&out.stderr));
    }
    Ok(out.stdout.trim().to_string())
}

static CLIXML_ERROR: LazyLock<Regex> = LazyLock::new(|| Regex::new(r#"<S S="Error">(.*?)</S>"#).expect("regex"));

/// Extrai texto legível do stderr (que pode vir em formato CLIXML).
pub fn clean_stderr(stderr: &str) -> String {
    let text = if stderr.trim_start().starts_with("#< CLIXML") {
        CLIXML_ERROR
            .captures_iter(stderr)
            .map(|c| c[1].replace("_x000D__x000A_", " ").replace("_x000A_", " "))
            .collect::<Vec<_>>()
            .join(" ")
    } else {
        stderr.to_string()
    };
    crate::util::truncate_chars(&crate::util::one_line(&text), 600)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encodes_utf16le_base64_with_preamble() {
        let encoded = encode_command("Write-Output 'olá'");
        let bytes = base64::engine::general_purpose::STANDARD.decode(encoded).unwrap();
        let units: Vec<u16> = bytes.chunks(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect();
        let text = String::from_utf16(&units).unwrap();
        assert!(text.starts_with("$ErrorActionPreference = 'Stop'"));
        assert!(text.ends_with("Write-Output 'olá'"));
    }

    #[test]
    fn cleans_clixml_errors() {
        let raw = "#< CLIXML\r\n<Objs Version=\"1.1.0.1\"><S S=\"Error\">Falha ao consultar_x000D__x000A_</S><S S=\"Error\">acesso negado_x000D__x000A_</S></Objs>";
        assert_eq!(clean_stderr(raw), "Falha ao consultar acesso negado");
        assert_eq!(clean_stderr("erro simples\n"), "erro simples");
    }
}
