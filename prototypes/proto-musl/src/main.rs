//! Prototype: does the core stack build into one static musl binary?
//! Throwaway code; see FINDINGS.md.

use std::io::Read;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use portable_pty::{CommandBuilder, PtySize, native_pty_system};

#[derive(Parser)]
struct Cli {
    #[command(subcommand)]
    action: Action,
}

#[derive(Subcommand)]
enum Action {
    /// HTTPS GET and print status plus body length.
    Https { url: String },
    /// List containers via the local Docker socket.
    Docker,
    /// Run a command under a PTY and print its output.
    Pty {
        #[arg(required = true, trailing_var_arg = true)]
        command: Vec<String>,
    },
}

fn main() -> Result<()> {
    match Cli::parse().action {
        Action::Https { url } => runtime()?.block_on(https(&url)),
        Action::Docker => runtime()?.block_on(docker()),
        Action::Pty { command } => pty(&command),
    }
}

fn runtime() -> Result<tokio::runtime::Runtime> {
    Ok(tokio::runtime::Builder::new_multi_thread().enable_all().build()?)
}

async fn https(url: &str) -> Result<()> {
    let resp = reqwest::get(url).await.context("request failed")?;
    let status = resp.status();
    let body = resp.bytes().await?;
    println!("status={status} bytes={}", body.len());
    Ok(())
}

async fn docker() -> Result<()> {
    let docker = bollard::Docker::connect_with_local_defaults()?;
    let containers = docker.list_containers(None).await?;
    for c in containers {
        let name = c.names.unwrap_or_default().join(",");
        println!("{name}\t{}", c.status.unwrap_or_default());
    }
    Ok(())
}

fn pty(command: &[String]) -> Result<()> {
    let pair = native_pty_system().openpty(PtySize {
        rows: 24,
        cols: 80,
        pixel_width: 0,
        pixel_height: 0,
    })?;
    let mut cmd = CommandBuilder::new(&command[0]);
    cmd.args(&command[1..]);
    let mut child = pair.slave.spawn_command(cmd)?;
    drop(pair.slave);

    let mut reader = pair.master.try_clone_reader()?;
    let mut out = Vec::new();
    // EIO at EOF is how Linux PTYs signal the child closed its side.
    if let Err(e) = reader.read_to_end(&mut out)
        && e.raw_os_error() != Some(5)
    {
        return Err(e.into());
    }
    let status = child.wait()?;
    print!("{}", String::from_utf8_lossy(&out));
    println!("[exit success={}]", status.success());
    Ok(())
}
