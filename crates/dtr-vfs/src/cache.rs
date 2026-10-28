//! Where a fetched remote file lands.
//!
//! One directory per machine, mirroring the remote tree, so the program that
//! opens the copy sees a recognisable name (`…/jupyter/var/log/syslog`) rather
//! than a hash. `..` is dropped rather than resolved: nothing a remote program
//! prints can address a path outside the cache root.

use std::path::{Component, Path, PathBuf};

use crate::BackendId;

/// The cache path for a remote file. Pure, so the escaping is testable without
/// a filesystem.
pub fn path_for(root: &Path, id: &BackendId, path: &Path) -> PathBuf {
    let mut out = root.join(match id {
        BackendId::Local => "local".to_owned(),
        BackendId::Ssh(alias) => sanitise(alias),
    });
    let mut named = false;
    for component in path.components() {
        match component {
            Component::Normal(part) => {
                out.push(sanitise(&part.to_string_lossy()));
                named = true;
            }
            // A root, a prefix or a `..` says nothing about where the file is
            // *inside* the cache; the parts around it do.
            Component::RootDir | Component::Prefix(_) | Component::CurDir | Component::ParentDir => {}
        }
    }
    if !named {
        out.push("file");
    }
    out
}

/// Make a directory private, creating the parents.
///
/// A fetched file is somebody's remote data; the cache is per-user, but a
/// liberal umask would still leave it readable to the machine. `0700` on the
/// directories and `0600` on the files closes that without a config knob.
pub fn ensure_dir(path: &Path) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        let mut builder = std::fs::DirBuilder::new();
        builder.recursive(true).mode(0o700);
        builder.create(path)
    }
    #[cfg(not(unix))]
    std::fs::create_dir_all(path)
}

/// Narrow a fetched file to its owner.
pub fn restrict(path: &Path) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
    }
    #[cfg(not(unix))]
    {
        let _ = path;
        Ok(())
    }
}

/// Keep a directory name to letters, digits and the few separators that make a
/// path readable; an alias or a file name is not trusted to be a name.
fn sanitise(text: &str) -> String {
    let mut out: String = text
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.') {
                ch
            } else {
                '_'
            }
        })
        .collect();
    if out.is_empty() || out == "." || out == ".." {
        out = format!("_{out}");
    }
    out
}

#[cfg(test)]
mod tests {
    use super::path_for;
    use crate::BackendId;
    use std::path::{Path, PathBuf};

    fn root() -> PathBuf {
        PathBuf::from("/cache")
    }

    #[test]
    fn a_remote_path_mirrors_under_its_machine() {
        assert_eq!(
            path_for(&root(), &BackendId::Ssh("jupyter".into()), Path::new("/var/log/syslog")),
            PathBuf::from("/cache/jupyter/var/log/syslog")
        );
    }

    /// Nothing a remote program prints can climb out of the cache.
    #[test]
    fn parent_components_cannot_escape() {
        assert_eq!(
            path_for(
                &root(),
                &BackendId::Ssh("jupyter".into()),
                Path::new("../../etc/passwd")
            ),
            PathBuf::from("/cache/jupyter/etc/passwd")
        );
    }

    /// An alias comes from `~/.ssh/config`, so it is not a directory name.
    #[test]
    fn an_alias_is_sanitised() {
        assert_eq!(
            path_for(&root(), &BackendId::Ssh("lab/../jup iter".into()), Path::new("/x")),
            PathBuf::from("/cache/lab_.._jup_iter/x")
        );
    }
}
