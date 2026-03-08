use std::path::Path;

use tokio::process::Command;

/// Initialize a workspace directory.
/// Called as an init container before the agent starts.
/// Matches the Go agent's `init` command behavior.
pub async fn initialize_workspace(workspace_dir: &str, source_dir: &str) -> Result<(), InitError> {
    let path = Path::new(workspace_dir);

    // Ensure workspace directory exists
    if !path.exists() {
        tokio::fs::create_dir_all(path)
            .await
            .map_err(|e| InitError::Io {
                detail: format!("failed to create workspace dir: {}", e),
            })?;
        tracing::info!(dir = %workspace_dir, "created workspace directory");
    }

    // Read pod annotations (env vars set by init container) to determine source type
    let source = determine_source_from_env();

    match source {
        InitSource::Git { url, revision, dir } => {
            tracing::info!(%url, "cloning git repository");
            clone_git_repo(source_dir, &url, revision.as_deref()).await?;
            symlink_subdir(source_dir, workspace_dir, dir.as_deref()).await?;
        }
        InitSource::Flux { url, digest, dir } => {
            tracing::info!(%url, "fetching Flux artifact");
            fetch_flux_artifact(source_dir, &url, digest.as_deref()).await?;
            symlink_subdir(source_dir, workspace_dir, dir.as_deref()).await?;
        }
        InitSource::Program { url } => {
            tracing::info!(%url, "fetching program from file server");
            fetch_program(source_dir, &url).await?;
            symlink_subdir(source_dir, workspace_dir, None).await?;
        }
        InitSource::Local => {
            tracing::info!("using local source, no initialization needed");
        }
    }

    tracing::info!(dir = %workspace_dir, "workspace initialized");
    Ok(())
}

#[derive(Debug, thiserror::Error)]
pub enum InitError {
    #[error("io: {detail}")]
    Io { detail: String },
    #[error("git: {detail}")]
    Git { detail: String },
    #[error("fetch: {detail}")]
    Fetch { detail: String },
    #[error("invalid subdir: {subdir}")]
    InvalidSubdir { subdir: String },
}

#[derive(Debug)]
enum InitSource {
    Git {
        url: String,
        revision: Option<String>,
        dir: Option<String>,
    },
    Flux {
        url: String,
        digest: Option<String>,
        dir: Option<String>,
    },
    Program {
        url: String,
    },
    Local,
}

/// Determine source type from environment variables.
/// The workspace controller sets these on the fetch init container.
fn determine_source_from_env() -> InitSource {
    if let Ok(url) = std::env::var("GIT_URL") {
        return InitSource::Git {
            url,
            revision: std::env::var("GIT_REVISION").ok(),
            dir: std::env::var("GIT_DIR").ok(),
        };
    }

    if let Ok(url) = std::env::var("FLUX_URL") {
        return InitSource::Flux {
            url,
            digest: std::env::var("FLUX_DIGEST").ok(),
            dir: std::env::var("FLUX_DIR").ok(),
        };
    }

    if let Ok(url) = std::env::var("PROGRAM_URL") {
        return InitSource::Program { url };
    }

    InitSource::Local
}

/// Clone a git repository to the target directory.
async fn clone_git_repo(
    target_dir: &str,
    url: &str,
    revision: Option<&str>,
) -> Result<(), InitError> {
    // git clone --depth 1
    let mut args = vec!["clone", "--depth", "1"];
    if let Some(rev) = revision {
        args.extend(["--branch", rev]);
    }
    args.extend([url, target_dir]);

    let output = Command::new("git")
        .args(&args)
        .output()
        .await
        .map_err(|e| InitError::Git {
            detail: format!("failed to run git clone: {}", e),
        })?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(InitError::Git {
            detail: format!("git clone failed: {}", stderr),
        });
    }

    // If revision is a commit hash (not a branch), do a fetch + checkout
    if let Some(rev) = revision {
        if rev.len() >= 7 && rev.chars().all(|c| c.is_ascii_hexdigit()) {
            let fetch_out = Command::new("git")
                .args(["fetch", "origin", rev])
                .current_dir(target_dir)
                .output()
                .await
                .map_err(|e| InitError::Git {
                    detail: format!("failed to fetch commit: {}", e),
                })?;

            if fetch_out.status.success() {
                let checkout = Command::new("git")
                    .args(["checkout", rev])
                    .current_dir(target_dir)
                    .output()
                    .await
                    .map_err(|e| InitError::Git {
                        detail: format!("failed to run git checkout: {}", e),
                    })?;
                if !checkout.status.success() {
                    let stderr = String::from_utf8_lossy(&checkout.stderr);
                    return Err(InitError::Git {
                        detail: format!("git checkout {} failed: {}", rev, stderr),
                    });
                }
            }
        }
    }

    tracing::info!(target_dir, "git clone completed");
    Ok(())
}

/// Fetch a tar.gz archive from a URL and extract it into target_dir.
/// Shared implementation for Flux artifacts and Program sources.
async fn fetch_and_extract(target_dir: &str, url: &str, label: &str) -> Result<(), InitError> {
    tokio::fs::create_dir_all(target_dir)
        .await
        .map_err(|e| InitError::Io {
            detail: format!("failed to create target dir: {}", e),
        })?;

    let response = reqwest::get(url).await.map_err(|e| InitError::Fetch {
        detail: format!("failed to fetch {}: {}", label, e),
    })?;

    if !response.status().is_success() {
        return Err(InitError::Fetch {
            detail: format!("{} fetch returned {}", label, response.status()),
        });
    }

    let bytes = response.bytes().await.map_err(|e| InitError::Fetch {
        detail: format!("failed to read {} body: {}", label, e),
    })?;

    let decoder = flate2::read::GzDecoder::new(&bytes[..]);
    let mut archive = tar::Archive::new(decoder);
    archive.unpack(target_dir).map_err(|e| InitError::Fetch {
        detail: format!("failed to extract {}: {}", label, e),
    })?;

    tracing::info!(target_dir, label, "archive extracted");
    Ok(())
}

/// Fetch a Flux source artifact (tar.gz) and extract it.
async fn fetch_flux_artifact(
    target_dir: &str,
    url: &str,
    _digest: Option<&str>,
) -> Result<(), InitError> {
    fetch_and_extract(target_dir, url, "artifact").await
}

/// Fetch a program from the operator's file server (tar.gz) and extract it.
async fn fetch_program(target_dir: &str, url: &str) -> Result<(), InitError> {
    fetch_and_extract(target_dir, url, "program").await
}

/// Create a symlink from source_dir/subdir → workspace_dir.
/// If no subdir, symlink source_dir → workspace_dir directly.
/// Matches Go agent behavior: `ln -s /share/source/{dir} /share/workspace`.
async fn symlink_subdir(
    source_dir: &str,
    workspace_dir: &str,
    subdir: Option<&str>,
) -> Result<(), InitError> {
    let source = match subdir {
        Some(d) if !d.is_empty() => {
            // Defense-in-depth: reject path traversal even though subdir
            // comes from controller-set annotations.
            if d.contains("..") || d.starts_with('/') {
                return Err(InitError::InvalidSubdir {
                    subdir: d.to_owned(),
                });
            }
            format!("{}/{}", source_dir, d)
        }
        _ => source_dir.to_owned(),
    };

    // Remove workspace_dir if it already exists (init retry or pre-created directory)
    let ws = Path::new(workspace_dir);
    if ws.read_link().is_ok() {
        let _ = tokio::fs::remove_file(ws).await;
    } else if ws.is_dir() {
        let _ = tokio::fs::remove_dir_all(ws).await;
    } else if ws.exists() {
        let _ = tokio::fs::remove_file(ws).await;
    }

    tokio::fs::symlink(&source, workspace_dir)
        .await
        .map_err(|e| InitError::Io {
            detail: format!(
                "failed to create symlink {} → {}: {}",
                workspace_dir, source, e
            ),
        })?;

    tracing::debug!(source, workspace_dir, "symlinked source to workspace");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serial_test::serial;
    use std::path::PathBuf;

    // -- determine_source_from_env tests --

    fn clear_source_env() {
        std::env::remove_var("GIT_URL");
        std::env::remove_var("GIT_REVISION");
        std::env::remove_var("GIT_DIR");
        std::env::remove_var("FLUX_URL");
        std::env::remove_var("FLUX_DIGEST");
        std::env::remove_var("FLUX_DIR");
        std::env::remove_var("PROGRAM_URL");
    }

    #[test]
    #[serial]
    fn determine_source_local_when_no_vars() {
        clear_source_env();
        match determine_source_from_env() {
            InitSource::Local => {}
            other => panic!("expected Local, got {:?}", other),
        }
    }

    #[test]
    #[serial]
    fn determine_source_git_full() {
        clear_source_env();
        std::env::set_var("GIT_URL", "https://github.com/test/repo");
        std::env::set_var("GIT_REVISION", "main");
        std::env::set_var("GIT_DIR", "infra");
        match determine_source_from_env() {
            InitSource::Git { url, revision, dir } => {
                assert_eq!(url, "https://github.com/test/repo");
                assert_eq!(revision.as_deref(), Some("main"));
                assert_eq!(dir.as_deref(), Some("infra"));
            }
            other => panic!("expected Git, got {:?}", other),
        }
        clear_source_env();
    }

    #[test]
    #[serial]
    fn determine_source_git_minimal() {
        clear_source_env();
        std::env::set_var("GIT_URL", "https://github.com/test/repo");
        match determine_source_from_env() {
            InitSource::Git { url, revision, dir } => {
                assert_eq!(url, "https://github.com/test/repo");
                assert!(revision.is_none());
                assert!(dir.is_none());
            }
            other => panic!("expected Git, got {:?}", other),
        }
        clear_source_env();
    }

    #[test]
    #[serial]
    fn determine_source_flux() {
        clear_source_env();
        std::env::set_var("FLUX_URL", "http://source-controller/artifact.tar.gz");
        std::env::set_var("FLUX_DIGEST", "sha256:abc");
        std::env::set_var("FLUX_DIR", "deploy");
        match determine_source_from_env() {
            InitSource::Flux { url, digest, dir } => {
                assert_eq!(url, "http://source-controller/artifact.tar.gz");
                assert_eq!(digest.as_deref(), Some("sha256:abc"));
                assert_eq!(dir.as_deref(), Some("deploy"));
            }
            other => panic!("expected Flux, got {:?}", other),
        }
        clear_source_env();
    }

    #[test]
    #[serial]
    fn determine_source_program() {
        clear_source_env();
        std::env::set_var(
            "PROGRAM_URL",
            "http://operator:8080/programs/my-prog.tar.gz",
        );
        match determine_source_from_env() {
            InitSource::Program { url } => {
                assert_eq!(url, "http://operator:8080/programs/my-prog.tar.gz");
            }
            other => panic!("expected Program, got {:?}", other),
        }
        clear_source_env();
    }

    #[test]
    #[serial]
    fn determine_source_git_priority_over_flux() {
        clear_source_env();
        std::env::set_var("GIT_URL", "https://github.com/test/repo");
        std::env::set_var("FLUX_URL", "http://source-controller/artifact.tar.gz");
        match determine_source_from_env() {
            InitSource::Git { .. } => {}
            other => panic!("expected Git (priority), got {:?}", other),
        }
        clear_source_env();
    }

    // -- symlink_subdir tests --

    #[tokio::test]
    async fn symlink_basic() {
        let tmp = tempfile::tempdir().unwrap();
        let source = tmp.path().join("source");
        let workspace = tmp.path().join("workspace");
        std::fs::create_dir_all(&source).unwrap();
        std::fs::write(source.join("main.py"), "print('hi')").unwrap();

        symlink_subdir(source.to_str().unwrap(), workspace.to_str().unwrap(), None)
            .await
            .unwrap();

        assert!(workspace.is_symlink());
        let target = std::fs::read_link(&workspace).unwrap();
        assert_eq!(target, source);
    }

    #[tokio::test]
    async fn symlink_with_subdir() {
        let tmp = tempfile::tempdir().unwrap();
        let source = tmp.path().join("source");
        let subdir = source.join("infra");
        let workspace = tmp.path().join("workspace");
        std::fs::create_dir_all(&subdir).unwrap();

        symlink_subdir(
            source.to_str().unwrap(),
            workspace.to_str().unwrap(),
            Some("infra"),
        )
        .await
        .unwrap();

        assert!(workspace.is_symlink());
        let target = std::fs::read_link(&workspace).unwrap();
        let expected = PathBuf::from(format!("{}/infra", source.display()));
        assert_eq!(target, expected);
    }

    #[tokio::test]
    async fn symlink_replaces_existing() {
        let tmp = tempfile::tempdir().unwrap();
        let source1 = tmp.path().join("source1");
        let source2 = tmp.path().join("source2");
        let workspace = tmp.path().join("workspace");
        std::fs::create_dir_all(&source1).unwrap();
        std::fs::create_dir_all(&source2).unwrap();

        // Create initial symlink
        symlink_subdir(source1.to_str().unwrap(), workspace.to_str().unwrap(), None)
            .await
            .unwrap();
        assert_eq!(std::fs::read_link(&workspace).unwrap(), source1);

        // Replace with new symlink
        symlink_subdir(source2.to_str().unwrap(), workspace.to_str().unwrap(), None)
            .await
            .unwrap();
        assert_eq!(std::fs::read_link(&workspace).unwrap(), source2);
    }

    // -- InitError tests --

    #[test]
    fn init_error_display_io() {
        let e = InitError::Io {
            detail: "disk full".into(),
        };
        assert_eq!(format!("{e}"), "io: disk full");
    }

    #[test]
    fn init_error_display_git() {
        let e = InitError::Git {
            detail: "auth failed".into(),
        };
        assert_eq!(format!("{e}"), "git: auth failed");
    }

    #[test]
    fn init_error_display_fetch() {
        let e = InitError::Fetch {
            detail: "404".into(),
        };
        assert_eq!(format!("{e}"), "fetch: 404");
    }

    #[test]
    fn init_error_is_std_error() {
        let e: Box<dyn std::error::Error> = Box::new(InitError::Io {
            detail: "test".into(),
        });
        assert!(e.source().is_none());
    }
}
