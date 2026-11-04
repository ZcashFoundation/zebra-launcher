//! Locates the generated, offline Zebra documentation shipped as resources.

use std::{
    fs,
    path::{Path, PathBuf},
};

use launcher_types::{NodeDocs, ZebradChannel};
use tauri::{AppHandle, Manager, Runtime};

const RESOURCE_DIR: &str = "zebra-docs";

/// Resolves and validates the three documents for `channel`, then allows the
/// asset protocol to serve their fixed resource tree to the docs iframe.
pub(crate) fn load<R: Runtime>(
    app: &AppHandle<R>,
    channel: ZebradChannel,
) -> Result<NodeDocs, String> {
    let root = resource_root(app)?;
    let docs = resolve(&root, channel)?;
    app.asset_protocol_scope()
        .allow_directory(&root, true)
        .map_err(|err| {
            format!(
                "could not allow the offline Zebra documentation at {} to load: {err}",
                root.display()
            )
        })?;
    Ok(docs)
}

/// Bundles install resources under Tauri's resource directory. Development
/// builds read them straight from the source tree instead.
fn resource_root<R: Runtime>(app: &AppHandle<R>) -> Result<PathBuf, String> {
    #[cfg(debug_assertions)]
    {
        let _ = app;
        Ok(PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("resources")
            .join(RESOURCE_DIR))
    }
    #[cfg(not(debug_assertions))]
    {
        app.path()
            .resource_dir()
            .map(|dir| dir.join(RESOURCE_DIR))
            .map_err(|err| format!("could not locate the bundled Zebra documentation: {err}"))
    }
}

fn resolve(root: &Path, channel: ZebradChannel) -> Result<NodeDocs, String> {
    let root = fs::canonicalize(root).map_err(|err| {
        format!(
            "the offline Zebra documentation is unavailable at {}: {err}; reinstall Zeal, or restore the development resources under src-tauri/resources/zebra-docs",
            root.display()
        )
    })?;
    let channel_dir = root.join(channel.as_str());
    let book = document(
        &root,
        &channel_dir.join("book/index.html"),
        &channel,
        "book",
    )?;
    let config = document(
        &root,
        &channel_dir.join("config.html"),
        &channel,
        "configuration",
    )?;
    let changes = document(
        &root,
        &channel_dir.join("changes.html"),
        &channel,
        "changes",
    )?;
    Ok(NodeDocs {
        channel,
        book,
        config,
        changes,
    })
}

fn document(
    root: &Path,
    path: &Path,
    channel: &ZebradChannel,
    section: &str,
) -> Result<String, String> {
    let resolved = fs::canonicalize(path).map_err(|err| {
        format!(
            "the offline Zebra {} {section} documentation is unavailable at {}: {err}",
            channel.as_str(),
            path.display()
        )
    })?;
    if !resolved.starts_with(root) {
        return Err(format!(
            "the offline Zebra {} {section} documentation at {} resolves outside the bundled documentation directory",
            channel.as_str(),
            path.display()
        ));
    }
    if !resolved.is_file() {
        return Err(format!(
            "the offline Zebra {} {section} documentation at {} is not a file",
            channel.as_str(),
            resolved.display()
        ));
    }
    resolved.into_os_string().into_string().map_err(|path| {
        format!(
            "the offline Zebra documentation path {} is not valid UTF-8",
            PathBuf::from(path).display()
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_document(root: &Path, channel: ZebradChannel, relative: &str) {
        let path = root.join(channel.as_str()).join(relative);
        fs::create_dir_all(path.parent().expect("document parent")).expect("create docs directory");
        fs::write(path, "<!doctype html>").expect("write document");
    }

    #[test]
    fn resolves_fixed_documents_for_each_channel() {
        let temp = tempfile::tempdir().expect("tempdir");
        for channel in ZebradChannel::ALL {
            let name = channel.as_str();
            for document in ["book/index.html", "config.html", "changes.html"] {
                write_document(temp.path(), channel, document);
            }

            let docs = resolve(temp.path(), channel).expect("resolve docs");
            let root = fs::canonicalize(temp.path())
                .expect("canonical root")
                .join(name);
            assert_eq!(docs.channel, channel);
            assert_eq!(docs.book, root.join("book/index.html").to_string_lossy());
            assert_eq!(docs.config, root.join("config.html").to_string_lossy());
            assert_eq!(docs.changes, root.join("changes.html").to_string_lossy());
            assert!(Path::new(&docs.book).is_absolute());
            assert!(Path::new(&docs.config).is_absolute());
            assert!(Path::new(&docs.changes).is_absolute());
        }
    }

    #[test]
    fn missing_documents_have_a_useful_error() {
        let temp = tempfile::tempdir().expect("tempdir");
        let error = resolve(temp.path(), ZebradChannel::Nightly).expect_err("missing docs");
        assert!(error.contains("nightly"), "{error}");
        assert!(error.contains("book"), "{error}");
        assert!(error.contains("index.html"), "{error}");
        assert!(error.contains("unavailable"), "{error}");
    }

    #[cfg(unix)]
    #[test]
    fn rejects_documents_outside_the_resource_tree() {
        use std::os::unix::fs::symlink;

        let temp = tempfile::tempdir().expect("tempdir");
        let root = temp.path().join("resources");
        for document in ["book/index.html", "config.html", "changes.html"] {
            write_document(&root, ZebradChannel::Beta, document);
        }
        let outside = temp.path().join("outside.html");
        fs::write(&outside, "<!doctype html>").expect("write outside document");
        let book = root.join("beta/book/index.html");
        fs::remove_file(&book).expect("remove bundled document");
        symlink(&outside, &book).expect("symlink document");

        let error = resolve(&root, ZebradChannel::Beta).expect_err("reject symlink");
        assert!(error.contains("outside"), "{error}");
        assert!(error.contains("beta"), "{error}");
    }
}
