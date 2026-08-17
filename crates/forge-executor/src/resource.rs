//! Portable disk-capacity preflight and active emergency monitoring.

use std::path::{Path, PathBuf};
use std::time::Duration;

use forge_core::run::{InfrastructureFailure, InfrastructureFailureKind};

use crate::error::{ExecError, ExecResult};

/// One filesystem-capacity observation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DiskCapacity {
    pub available_bytes: u64,
    pub total_bytes: u64,
}

impl DiskCapacity {
    pub fn free_percent(self) -> f64 {
        if self.total_bytes == 0 {
            0.0
        } else {
            self.available_bytes as f64 * 100.0 / self.total_bytes as f64
        }
    }
}

/// Absolute and relative floors required before an expensive run starts.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct DiskPreflightPolicy {
    pub minimum_free_bytes: u64,
    pub minimum_free_percent: f64,
}

impl DiskPreflightPolicy {
    pub fn check(self, path: &Path, capacity: DiskCapacity) -> ExecResult<()> {
        let percent = capacity.free_percent();
        if capacity.available_bytes < self.minimum_free_bytes || percent < self.minimum_free_percent
        {
            return Err(ExecError::Infrastructure(InfrastructureFailure::new(
                InfrastructureFailureKind::DiskExhausted,
                format!(
                    "volume for `{}` has {} bytes free ({percent:.2}%); requires at least {} bytes and {:.2}%",
                    path.display(),
                    capacity.available_bytes,
                    self.minimum_free_bytes,
                    self.minimum_free_percent
                ),
            )));
        }
        Ok(())
    }
}

/// Active emergency-floor monitor attached to a subprocess.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiskWatch {
    pub paths: Vec<PathBuf>,
    pub emergency_free_bytes: u64,
    pub interval: Duration,
    pub workspace_limit: Option<(PathBuf, u64)>,
}

impl DiskWatch {
    pub fn new(
        paths: impl IntoIterator<Item = PathBuf>,
        emergency_free_bytes: u64,
        interval: Duration,
    ) -> Self {
        Self {
            paths: paths.into_iter().collect(),
            emergency_free_bytes,
            interval,
            workspace_limit: None,
        }
    }

    pub fn with_workspace_limit(mut self, root: impl Into<PathBuf>, maximum_bytes: u64) -> Self {
        self.workspace_limit = Some((root.into(), maximum_bytes));
        self
    }

    pub fn check(&self) -> ExecResult<()> {
        for path in &self.paths {
            let capacity = capacity(path)?;
            if capacity.available_bytes < self.emergency_free_bytes {
                return Err(ExecError::Infrastructure(InfrastructureFailure::new(
                    InfrastructureFailureKind::DiskExhausted,
                    format!(
                        "disk watchdog observed {} bytes free for `{}`, below emergency floor {}",
                        capacity.available_bytes,
                        path.display(),
                        self.emergency_free_bytes
                    ),
                )));
            }
        }
        if let Some((root, limit)) = &self.workspace_limit {
            let used = directory_size(root)?;
            if used > *limit {
                return Err(ExecError::Infrastructure(InfrastructureFailure::new(
                    InfrastructureFailureKind::DiskExhausted,
                    format!(
                        "workspace `{}` uses {used} bytes, above configured limit {limit}",
                        root.display()
                    ),
                )));
            }
        }
        Ok(())
    }
}

/// Checks every configured path using filesystem APIs rather than parsing
/// platform-specific `df` output.
pub fn preflight_disk(
    paths: impl IntoIterator<Item = PathBuf>,
    policy: DiskPreflightPolicy,
) -> ExecResult<Vec<(PathBuf, DiskCapacity)>> {
    let mut observations = Vec::new();
    for path in paths {
        let observation = capacity(&path)?;
        policy.check(&path, observation)?;
        observations.push((path, observation));
    }
    Ok(observations)
}

pub fn capacity(path: &Path) -> ExecResult<DiskCapacity> {
    let existing = nearest_existing(path).ok_or_else(|| ExecError::Io {
        context: format!(
            "finding an existing volume ancestor for `{}`",
            path.display()
        ),
        source: std::io::Error::new(std::io::ErrorKind::NotFound, "no existing ancestor"),
    })?;
    let available_bytes = fs2::available_space(existing).map_err(|source| ExecError::Io {
        context: format!("checking free space for `{}`", path.display()),
        source,
    })?;
    let total_bytes = fs2::total_space(existing).map_err(|source| ExecError::Io {
        context: format!("checking volume size for `{}`", path.display()),
        source,
    })?;
    Ok(DiskCapacity {
        available_bytes,
        total_bytes,
    })
}

fn nearest_existing(path: &Path) -> Option<&Path> {
    path.ancestors().find(|candidate| candidate.exists())
}

fn directory_size(root: &Path) -> ExecResult<u64> {
    let mut total = 0_u64;
    let mut pending = vec![root.to_path_buf()];
    while let Some(path) = pending.pop() {
        let metadata = std::fs::symlink_metadata(&path).map_err(|source| ExecError::Io {
            context: format!("measuring workspace path `{}`", path.display()),
            source,
        })?;
        if metadata.file_type().is_symlink() {
            continue;
        }
        if metadata.is_file() {
            total = total.saturating_add(metadata.len());
        } else if metadata.is_dir() {
            for entry in std::fs::read_dir(&path).map_err(|source| ExecError::Io {
                context: format!("measuring workspace directory `{}`", path.display()),
                source,
            })? {
                pending.push(
                    entry
                        .map_err(|source| ExecError::Io {
                            context: format!("reading workspace directory `{}`", path.display()),
                            source,
                        })?
                        .path(),
                );
            }
        }
    }
    Ok(total)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn either_absolute_or_percentage_floor_fails_closed() {
        let policy = DiskPreflightPolicy {
            minimum_free_bytes: 100,
            minimum_free_percent: 10.0,
        };
        let path = Path::new("/volume");
        assert!(
            policy
                .check(
                    path,
                    DiskCapacity {
                        available_bytes: 99,
                        total_bytes: 1_000,
                    }
                )
                .is_err()
        );
        assert!(
            policy
                .check(
                    path,
                    DiskCapacity {
                        available_bytes: 100,
                        total_bytes: 2_000,
                    }
                )
                .is_err()
        );
        policy
            .check(
                path,
                DiskCapacity {
                    available_bytes: 200,
                    total_bytes: 1_000,
                },
            )
            .unwrap();
    }

    #[test]
    fn real_capacity_probe_accepts_a_nonexistent_descendant() {
        let temporary = tempfile::tempdir().unwrap();
        let observation = capacity(&temporary.path().join("not-created/yet")).unwrap();
        assert!(observation.available_bytes > 0);
        assert!(observation.total_bytes >= observation.available_bytes);
    }
}
