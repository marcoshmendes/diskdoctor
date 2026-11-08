//! Execução de processos externos com tempo limite.

use anyhow::{Context, Result, bail};
use std::ffi::OsStr;
use std::io::Read;
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

#[derive(Debug, Clone)]
pub struct ProcessOutput {
    pub code: Option<i32>,
    pub stdout: String,
    pub stderr: String,
}

/// Executa `program args...`, capturando stdout/stderr; mata o processo se exceder `timeout`.
pub fn run_with_timeout<S: AsRef<OsStr>>(program: impl AsRef<OsStr>, args: &[S], timeout: Duration) -> Result<ProcessOutput> {
    let program_name = program.as_ref().to_string_lossy().into_owned();
    let mut child = Command::new(program.as_ref())
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .with_context(|| format!("não foi possível executar {program_name}"))?;

    let mut stdout = child.stdout.take().expect("stdout capturado");
    let mut stderr = child.stderr.take().expect("stderr capturado");
    let out_reader = thread::spawn(move || {
        let mut buf = Vec::new();
        let _ = stdout.read_to_end(&mut buf);
        buf
    });
    let err_reader = thread::spawn(move || {
        let mut buf = Vec::new();
        let _ = stderr.read_to_end(&mut buf);
        buf
    });

    let started = Instant::now();
    let status = loop {
        if let Some(status) = child.try_wait()? {
            break status;
        }
        if started.elapsed() > timeout {
            let _ = child.kill();
            let _ = child.wait();
            bail!("{program_name} não respondeu em {} s e foi encerrado", timeout.as_secs());
        }
        thread::sleep(Duration::from_millis(20));
    };

    let stdout = out_reader.join().unwrap_or_default();
    let stderr = err_reader.join().unwrap_or_default();
    Ok(ProcessOutput {
        code: status.code(),
        stdout: strip_bom(String::from_utf8_lossy(&stdout).into_owned()),
        stderr: String::from_utf8_lossy(&stderr).into_owned(),
    })
}

fn strip_bom(s: String) -> String {
    s.strip_prefix('\u{feff}').map(str::to_string).unwrap_or(s)
}
