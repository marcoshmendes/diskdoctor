//! Runs PowerShell scripts that print JSON.
//!
//! Uses Windows PowerShell 5.1 (`powershell.exe`), present on every Windows 10/11, instead
//! of requiring PowerShell 7. The script goes through `-EncodedCommand` (UTF-16LE in
//! base64) to avoid any quoting/escaping problem, and the output is forced to UTF-8 to keep
//! accented characters in localized event messages.

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

/// Encodes the script (with the preamble) for `-EncodedCommand`.
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

/// Runs the script and returns its stdout (expected: JSON).
pub fn run_script(script: &str, timeout: Duration) -> Result<String> {
    let encoded = encode_command(script);
    let args = ["-NoProfile", "-NonInteractive", "-ExecutionPolicy", "Bypass", "-EncodedCommand", encoded.as_str()];
    let out = run_with_timeout(powershell_exe(), &args, timeout)?;
    if out.code != Some(0) {
        bail!(tr!("PowerShell returned an error: {}", "o PowerShell retornou erro: {}", clean_stderr(&out.stderr)));
    }
    Ok(out.stdout.trim().to_string())
}

static CLIXML_ERROR: LazyLock<Regex> = LazyLock::new(|| Regex::new(r#"<S S="Error">(.*?)</S>"#).expect("regex"));

/// Extracts readable text from stderr (which may come in CLIXML format).
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
        assert!(text.ends_with("Write-Output 'olá'"), "non-ASCII text survives the encoding");
    }

    #[test]
    fn cleans_clixml_errors() {
        let raw = "#< CLIXML\r\n<Objs Version=\"1.1.0.1\"><S S=\"Error\">Query failed_x000D__x000A_</S><S S=\"Error\">access denied_x000D__x000A_</S></Objs>";
        assert_eq!(clean_stderr(raw), "Query failed access denied");
        assert_eq!(clean_stderr("plain error\n"), "plain error");
    }
}
