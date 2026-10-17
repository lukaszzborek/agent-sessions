//! Mirrors `ssh` sources from their host into the data dir with rsync, so the indexer reads them
//! like local files.
use crate::config::Source;
use crate::index::literal_prefix;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

/// Runs rsync for `src` into `mirror` every `src.interval` seconds, forever. Logs only when the
/// outcome changes, so a VM that is down does not flood the log.
pub async fn sync(src: Source, mirror: PathBuf) {
    let Some(host) = src.ssh.clone() else { return };
    let args = rsync_args(&src, &host, &mirror);
    let mut last: Option<Option<String>> = None;
    loop {
        let err = run(&args, &mirror).await.err();
        if last.as_ref() != Some(&err) {
            match &err {
                Some(e) => eprintln!("source {}: rsync from {} failed: {}", src.name, host, e),
                None => eprintln!("source {}: syncing from {}", src.name, host),
            }
            last = Some(err);
        }
        tokio::time::sleep(Duration::from_secs(src.interval.max(1))).await;
    }
}

async fn run(args: &[String], mirror: &Path) -> Result<(), String> {
    tokio::fs::create_dir_all(mirror)
        .await
        .map_err(|e| format!("create {}: {}", mirror.display(), e))?;
    let out = tokio::process::Command::new("rsync")
        .args(args)
        .stdin(Stdio::null())
        .output()
        .await
        .map_err(|e| format!("cannot run rsync: {e}"))?;
    match out.status.code() {
        // 23/24: some paths missing or vanished on the host, e.g. an agent not installed there.
        Some(0 | 23 | 24) => Ok(()),
        _ => Err(format!(
            "{}: {}",
            out.status,
            String::from_utf8_lossy(&out.stderr).trim()
        )),
    }
}

/// Copies only the literal dirs of the agent patterns, keeping `*.jsonl` and the meta file.
fn rsync_args(src: &Source, host: &str, mirror: &Path) -> Vec<String> {
    let mut rels: Vec<PathBuf> = [&src.claude, &src.codex, &src.pi]
        .into_iter()
        .flatten()
        .map(|p| literal_prefix(p))
        .collect();
    // Codex titles live in `session_index.jsonl` next to the sessions dir.
    if let Some(p) = &src.codex {
        let dir = literal_prefix(p).parent().map(Path::to_path_buf);
        rels.push(dir.unwrap_or_default().join("session_index.jsonl"));
    }
    let mut args: Vec<String> = [
        "--archive",
        "--relative",
        // Not --prune-empty-dirs: a dir emptied on the host would leave the transfer and its
        // mirrored files would never be deleted.
        "--delete",
        // A missing path counts as an I/O error, which would otherwise skip every deletion.
        "--ignore-errors",
        "--rsh=ssh -o BatchMode=yes -o ConnectTimeout=10 -o ServerAliveInterval=15 -o ServerAliveCountMax=2",
        "--include=*/",
        "--include=*.jsonl",
    ]
    .map(String::from)
    .into();
    args.extend(src.meta.iter().map(|m| format!("--include={m}")));
    args.push("--exclude=*".into());
    let base = src.path.to_string_lossy();
    for rel in &rels {
        let rel = rel.to_string_lossy();
        let rel = if rel.is_empty() { "." } else { &rel };
        // `/./` marks where --relative starts, so the mirror holds paths relative to `path`.
        args.push(if base.is_empty() {
            format!("{host}:{rel}")
        } else {
            format!("{host}:{base}/./{rel}")
        });
    }
    args.push(format!("{}/", mirror.display()));
    args
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn home_source_args() {
        let src = Source {
            ssh: Some("vm".into()),
            ..Source::home("vm", PathBuf::new())
        };
        let args = rsync_args(&src, "vm", Path::new("/m"));
        assert_eq!(
            args[args.len() - 6..],
            [
                "--exclude=*",
                "vm:.claude/projects",
                "vm:.codex/sessions",
                "vm:.pi/agent/sessions",
                "vm:.codex/session_index.jsonl",
                "/m/",
            ]
        );
    }

    #[test]
    fn glob_source_args() {
        let src = Source {
            name: "b".into(),
            path: "/srv/bench".into(),
            claude: Some("**/claude-projects".into()),
            codex: None,
            pi: None,
            meta: Some("meta.json".into()),
            enabled: true,
            ssh: Some("me@vm".into()),
            interval: 30,
        };
        let args = rsync_args(&src, "me@vm", Path::new("/m"));
        assert_eq!(
            args[args.len() - 4..],
            [
                "--include=meta.json",
                "--exclude=*",
                "me@vm:/srv/bench/./.",
                "/m/",
            ]
        );
    }
}
