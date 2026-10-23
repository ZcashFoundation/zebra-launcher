//! First-run data handling: where the chain state goes, how much room there
//! is, leftovers from the v0.0.0-alpha.4 prototype, and deleting the state.
//!
//! Everything here is plain blocking filesystem code; the Tauri commands run
//! it off the async runtime and check that zebrad is stopped first. Nothing
//! here ever copies the chain state: Mainnet state is hundreds of GB, so a
//! move is a same-volume rename or a message asking the user to move it.

use std::{
    fs, io,
    path::{Path, PathBuf},
};

use launcher_types::{FirstRunInfo, ImportOutcome, LegacyInstall, Settings};

use crate::paths::Paths;

/// How many directory entries [`dir_size`] looks at before it gives up and
/// reports a lower bound. A synced Mainnet state has tens of thousands of
/// files, so this is only reached by something unexpected.
pub const SIZE_WALK_LIMIT: usize = 500_000;

/// The name of the chain state directory, both in zebrad's cache directory
/// and in the launcher's data directories.
const STATE_DIR_NAME: &str = "state";

/// What the first-run flow shows. `legacy` comes from [`detect_legacy`].
pub fn first_run_info(
    paths: &Paths,
    settings: &Settings,
    legacy: Option<LegacyInstall>,
) -> FirstRunInfo {
    let free_bytes_at_default = match free_space(paths.data_dir()) {
        Ok(bytes) => Some(bytes),
        Err(err) => {
            tracing::warn!(%err, dir = %paths.data_dir().display(), "could not read the free space");
            None
        }
    };
    FirstRunInfo {
        is_first_run: !settings.first_run_done,
        default_data_dir: paths.data_dir().display().to_string(),
        free_bytes_at_default,
        legacy,
    }
}

/// Whether zebrad may be started: not before the first-run flow is done,
/// because until then the user has not chosen where the chain state goes.
///
/// The one rule for every start: at launch, from the tray, and the start
/// commands.
pub fn start_allowed(settings: &Settings) -> Result<(), String> {
    if settings.first_run_done {
        Ok(())
    } else {
        Err("finish the first-run setup before starting zebrad".to_string())
    }
}

/// The bytes available to this user on the volume holding `path`.
///
/// `path` does not need to exist yet: the nearest existing ancestor is
/// measured, which is the volume a new directory there would land on (unless
/// a mount point is created in between).
pub fn free_space(path: &Path) -> io::Result<u64> {
    let existing = nearest_existing(path)?;
    platform_free_space(&existing)
}

/// `path` or its closest ancestor that exists, as a directory.
fn nearest_existing(path: &Path) -> io::Result<PathBuf> {
    for ancestor in path.ancestors() {
        let ancestor = if ancestor.as_os_str().is_empty() {
            Path::new(".")
        } else {
            ancestor
        };
        if let Ok(meta) = fs::metadata(ancestor) {
            if meta.is_dir() {
                return Ok(ancestor.to_path_buf());
            }
            if let Some(parent) = ancestor.parent().filter(|p| !p.as_os_str().is_empty()) {
                return Ok(parent.to_path_buf());
            }
            return Ok(PathBuf::from("."));
        }
    }
    Err(io::Error::new(
        io::ErrorKind::NotFound,
        format!("no part of {} exists", path.display()),
    ))
}

#[cfg(unix)]
fn platform_free_space(dir: &Path) -> io::Result<u64> {
    let stat = nix::sys::statvfs::statvfs(dir).map_err(io::Error::from)?;
    // The field types differ between Linux and macOS.
    #[allow(clippy::unnecessary_cast)]
    let (blocks, size) = (stat.blocks_available() as u64, stat.fragment_size() as u64);
    Ok(blocks.saturating_mul(size))
}

#[cfg(windows)]
fn platform_free_space(dir: &Path) -> io::Result<u64> {
    use std::os::windows::ffi::OsStrExt;

    use windows_sys::Win32::Storage::FileSystem::GetDiskFreeSpaceExW;

    let wide: Vec<u16> = dir.as_os_str().encode_wide().chain(Some(0)).collect();
    let mut available: u64 = 0;
    // SAFETY: `wide` is NUL-terminated and outlives the call, `available` is
    // writable, and the two optional out pointers may be null.
    let ok = unsafe {
        GetDiskFreeSpaceExW(
            wide.as_ptr(),
            &mut available,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
        )
    };
    if ok == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(available)
}

/// Creates `dir` if needed and checks that the launcher can write there, by
/// creating and removing a probe file.
pub fn check_writable_dir(dir: &Path) -> io::Result<()> {
    if !dir.is_absolute() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("{} is not an absolute path", dir.display()),
        ));
    }
    fs::create_dir_all(dir)?;
    let probe = dir.join(format!(".zebra-launcher-probe-{}", std::process::id()));
    fs::write(&probe, b"probe")?;
    fs::remove_file(&probe)
}

/// The total size of the files under `dir`, without following symbolic
/// links, and whether the walk stopped at `limit` entries (so the total is a
/// lower bound). Unreadable entries are skipped. A missing `dir` is empty.
pub fn dir_size(dir: &Path, limit: usize) -> (u64, bool) {
    let mut total = 0u64;
    let mut seen = 0usize;
    let mut stack = vec![dir.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            seen += 1;
            if seen > limit {
                return (total, true);
            }
            // `DirEntry::metadata` does not follow symbolic links.
            let Ok(meta) = entry.metadata() else {
                continue;
            };
            if meta.is_dir() {
                stack.push(entry.path());
            } else if meta.is_file() {
                total = total.saturating_add(meta.len());
            }
        }
    }
    (total, false)
}

/// Where the v0.0.0-alpha.4 prototype left its files.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct LegacyLocations {
    /// The directory of the running executable: alpha.4 kept `zebrad.toml`
    /// next to it.
    pub exe_dir: Option<PathBuf>,
    /// zebrad's default `state.cache_dir`, `dirs::cache_dir()/zebra`, which
    /// alpha.4's zebrad used for the chain state.
    pub zebra_cache_dir: Option<PathBuf>,
}

impl LegacyLocations {
    /// The locations for this machine and user.
    pub fn current() -> Self {
        Self {
            exe_dir: std::env::current_exe()
                .ok()
                .and_then(|exe| exe.parent().map(Path::to_path_buf)),
            zebra_cache_dir: dirs::cache_dir().map(|dir| dir.join("zebra")),
        }
    }
}

/// `path` with symbolic links resolved, as far as it exists: the nearest
/// existing ancestor is canonicalized and the rest appended unchanged. Falls
/// back to `path` itself when nothing can be resolved.
fn resolve(path: &Path) -> PathBuf {
    for ancestor in path.ancestors() {
        if let Ok(resolved) = fs::canonicalize(ancestor) {
            return match path.strip_prefix(ancestor) {
                Ok(rest) if rest.as_os_str().is_empty() => resolved,
                Ok(rest) => resolved.join(rest),
                Err(_) => path.to_path_buf(),
            };
        }
    }
    path.to_path_buf()
}

/// Looks for alpha.4 leftovers: a `zebrad.toml` in `exe_dir`, or a
/// `zebra_cache_dir` holding a `state` directory. The state size is counted
/// up to [`SIZE_WALK_LIMIT`] entries.
///
/// `current_state_dir` is the launcher's own `state.cache_dir`: when it lies
/// inside the zebra cache directory (the user chose that location, possibly
/// through a symbolic link), the state there is the launcher's and is not
/// reported. Both paths are compared with links resolved.
pub fn detect_legacy(
    locations: &LegacyLocations,
    current_state_dir: &Path,
) -> Option<LegacyInstall> {
    let config_file = locations
        .exe_dir
        .as_deref()
        .map(|dir| dir.join("zebrad.toml"))
        .filter(|file| file.is_file());
    let current_state_dir = resolve(current_state_dir);
    let state_dir = locations
        .zebra_cache_dir
        .as_deref()
        .filter(|dir| dir.join(STATE_DIR_NAME).is_dir())
        .filter(|dir| !current_state_dir.starts_with(resolve(dir)))
        .map(Path::to_path_buf);
    if config_file.is_none() && state_dir.is_none() {
        return None;
    }
    let (state_bytes, state_bytes_is_lower_bound) = match &state_dir {
        Some(dir) => dir_size(&dir.join(STATE_DIR_NAME), SIZE_WALK_LIMIT),
        None => (0, false),
    };
    Some(LegacyInstall {
        config_file: config_file.map(|file| file.display().to_string()),
        state_dir: state_dir.map(|dir| dir.display().to_string()),
        state_bytes,
        state_bytes_is_lower_bound,
    })
}

/// Moves `<from>/state` (alpha.4's chain state in zebrad's cache directory)
/// to `<to>/state`, where `to` is the launcher's `state.cache_dir`.
///
/// The caller must make sure no zebrad uses either directory; the app only
/// allows this while its zebrad is stopped. The move is a rename, so it only
/// works within one volume. Across volumes it fails with
/// [`io::ErrorKind::CrossesDevices`] and a message asking the user to move
/// the folder by hand: the state is never copied behind the user's back.
/// It also refuses when `<to>/state` already holds something.
pub fn import_legacy_state(from: &Path, to: &Path) -> io::Result<ImportOutcome> {
    import_with(from, to, |source, target| fs::rename(source, target))
}

fn import_with(
    from: &Path,
    to: &Path,
    rename: impl FnOnce(&Path, &Path) -> io::Result<()>,
) -> io::Result<ImportOutcome> {
    let source = from.join(STATE_DIR_NAME);
    if !source.is_dir() {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            format!("there is no chain state at {}", source.display()),
        ));
    }
    let target = to.join(STATE_DIR_NAME);
    // Compared with links resolved, so a directory is never moved into itself
    // through a symbolic link.
    let (resolved_source, resolved_target) = (resolve(&source), resolve(&target));
    if resolved_target.starts_with(&resolved_source)
        || resolved_source.starts_with(&resolved_target)
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!(
                "{} and {} overlap; choose another data directory",
                source.display(),
                target.display()
            ),
        ));
    }
    match fs::symlink_metadata(&target) {
        Ok(meta) if meta.is_dir() && fs::read_dir(&target)?.next().is_none() => {
            fs::remove_dir(&target)?;
        }
        Ok(_) => {
            return Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                format!(
                    "{} already holds chain state; delete the node data first or leave the old state where it is",
                    target.display()
                ),
            ));
        }
        Err(err) if err.kind() == io::ErrorKind::NotFound => {}
        Err(err) => return Err(err),
    }
    fs::create_dir_all(to)?;
    match rename(&source, &target) {
        Ok(()) => {
            tracing::info!(from = %source.display(), to = %target.display(), "moved the alpha.4 chain state");
            Ok(ImportOutcome {
                from: source.display().to_string(),
                to: target.display().to_string(),
            })
        }
        Err(err) if is_cross_device(&err) => Err(io::Error::new(
            io::ErrorKind::CrossesDevices,
            format!(
                "{} is on another drive than {}, so it cannot be moved instantly, and the launcher does not copy hundreds of GB on its own. \
                 With the node stopped, move the folder by hand to {}, or choose a data directory on the same drive.",
                source.display(),
                to.display(),
                target.display()
            ),
        )),
        Err(err) => Err(err),
    }
}

/// True for the "rename across volumes" error of every platform.
fn is_cross_device(err: &io::Error) -> bool {
    if err.kind() == io::ErrorKind::CrossesDevices {
        return true;
    }
    #[cfg(unix)]
    let code = nix::errno::Errno::EXDEV as i32;
    #[cfg(windows)]
    let code = windows_sys::Win32::Foundation::ERROR_NOT_SAME_DEVICE as i32;
    err.raw_os_error() == Some(code)
}

/// Deletes the chain state directory `state_dir` and returns how many bytes
/// it held. Only the state goes: config, logs and the RPC cookie stay.
///
/// Refuses unless `state_dir` is named `state` and lies strictly inside one
/// of `allowed_roots` (the launcher's data directory and the data directory
/// chosen in the settings), after resolving symbolic links. A missing
/// directory frees 0 bytes. The caller makes sure zebrad is stopped.
pub fn delete_node_data(state_dir: &Path, allowed_roots: &[&Path]) -> io::Result<u64> {
    match fs::symlink_metadata(state_dir) {
        Ok(_) => {}
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(0),
        Err(err) => return Err(err),
    }
    let refuse = || {
        io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!(
                "refusing to delete {}: it is not a state directory inside the launcher's data directories",
                state_dir.display()
            ),
        )
    };
    if state_dir.file_name() != Some(STATE_DIR_NAME.as_ref()) {
        return Err(refuse());
    }
    let resolved = state_dir.canonicalize()?;
    let inside = allowed_roots.iter().any(|root| {
        root.canonicalize()
            .is_ok_and(|root| resolved.starts_with(&root) && resolved != root)
    });
    if !inside {
        return Err(refuse());
    }
    let (bytes, _) = dir_size(&resolved, usize::MAX);
    fs::remove_dir_all(&resolved)?;
    tracing::info!(dir = %resolved.display(), bytes, "deleted the node data");
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tempdir() -> tempfile::TempDir {
        tempfile::tempdir().expect("tempdir")
    }

    fn write_file(path: &Path, len: usize) {
        fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
        fs::write(path, vec![b'x'; len]).expect("write");
    }

    #[test]
    fn free_space_of_a_missing_path_uses_its_ancestor() {
        let dir = tempdir();
        let existing = free_space(dir.path()).expect("free space");
        assert!(existing > 0);
        let missing = free_space(&dir.path().join("a/b/c")).expect("free space of a new dir");
        assert!(missing > 0);
        // A file is measured through its directory.
        write_file(&dir.path().join("file"), 1);
        assert!(free_space(&dir.path().join("file")).expect("file") > 0);
        assert_eq!(
            nearest_existing(&dir.path().join("a/b")).expect("ancestor"),
            dir.path()
        );
    }

    #[test]
    fn first_run_follows_the_settings_marker() {
        let dir = tempdir();
        let paths = Paths::under(dir.path());
        paths.ensure_dirs().expect("dirs");
        let info = first_run_info(&paths, &Settings::default(), None);
        assert!(info.is_first_run);
        assert_eq!(
            info.default_data_dir,
            paths.data_dir().display().to_string()
        );
        assert!(info.free_bytes_at_default.is_some_and(|b| b > 0));
        let done = Settings {
            first_run_done: true,
            ..Settings::default()
        };
        assert!(!first_run_info(&paths, &done, None).is_first_run);
    }

    #[test]
    fn starts_wait_for_the_first_run() {
        assert!(start_allowed(&Settings::default()).is_err());
        let done = Settings {
            first_run_done: true,
            ..Settings::default()
        };
        assert_eq!(start_allowed(&done), Ok(()));
    }

    #[test]
    fn writable_dir_check() {
        let dir = tempdir();
        let new = dir.path().join("new/data");
        check_writable_dir(&new).expect("writable");
        assert!(new.is_dir());
        assert_eq!(fs::read_dir(&new).expect("list").count(), 0);
        let err = check_writable_dir(Path::new("relative/dir")).expect_err("relative");
        assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
    }

    #[test]
    fn dir_size_counts_files_and_stops_at_the_limit() {
        let dir = tempdir();
        write_file(&dir.path().join("a"), 10);
        write_file(&dir.path().join("sub/b"), 20);
        write_file(&dir.path().join("sub/deeper/c"), 30);
        assert_eq!(dir_size(dir.path(), usize::MAX), (60, false));
        let (partial, capped) = dir_size(dir.path(), 2);
        assert!(capped);
        assert!(partial < 60);
        assert_eq!(dir_size(&dir.path().join("missing"), 10), (0, false));
    }

    #[test]
    fn detects_alpha4_leftovers() {
        let dir = tempdir();
        let exe_dir = dir.path().join("app");
        let cache = dir.path().join("cache/zebra");
        fs::create_dir_all(&exe_dir).expect("mkdir");
        let locations = LegacyLocations {
            exe_dir: Some(exe_dir.clone()),
            zebra_cache_dir: Some(cache.clone()),
        };
        let ours = dir.path().join("launcher/data/state");
        assert_eq!(detect_legacy(&locations, &ours), None);

        // A cache dir without `state` (only a peer cache) is not reported.
        write_file(&cache.join("network/mainnet.peers"), 5);
        assert_eq!(detect_legacy(&locations, &ours), None);

        write_file(&cache.join("state/v27/mainnet/000001.sst"), 100);
        write_file(&exe_dir.join("zebrad.toml"), 3);
        let found = detect_legacy(&locations, &ours).expect("found");
        assert_eq!(
            found,
            LegacyInstall {
                config_file: Some(exe_dir.join("zebrad.toml").display().to_string()),
                state_dir: Some(cache.display().to_string()),
                state_bytes: 100,
                state_bytes_is_lower_bound: false,
            }
        );

        // The launcher's own state inside the zebra cache dir is not a leftover.
        let inside = detect_legacy(&locations, &cache.join("state")).expect("config only");
        assert_eq!(inside.state_dir, None);
        assert!(inside.config_file.is_some());
    }

    #[test]
    fn import_moves_the_state_directory() {
        let dir = tempdir();
        let from = dir.path().join("cache/zebra");
        let to = dir.path().join("launcher/data/state");
        write_file(&from.join("state/v27/mainnet/000001.sst"), 7);
        write_file(&from.join("network/mainnet.peers"), 1);
        // An empty target left by an earlier run is replaced.
        fs::create_dir_all(to.join("state")).expect("mkdir");

        let outcome = import_legacy_state(&from, &to).expect("import");
        assert_eq!(
            outcome,
            ImportOutcome {
                from: from.join("state").display().to_string(),
                to: to.join("state").display().to_string(),
            }
        );
        assert!(!from.join("state").exists());
        assert!(from.join("network/mainnet.peers").exists());
        assert_eq!(
            fs::read(to.join("state/v27/mainnet/000001.sst")).expect("moved"),
            vec![b'x'; 7]
        );

        // Nothing left to import.
        let err = import_legacy_state(&from, &to).expect_err("no state");
        assert_eq!(err.kind(), io::ErrorKind::NotFound);
    }

    #[test]
    fn import_refuses_a_non_empty_target() {
        let dir = tempdir();
        let from = dir.path().join("cache/zebra");
        let to = dir.path().join("data/state");
        write_file(&from.join("state/a"), 1);
        write_file(&to.join("state/b"), 1);
        let err = import_legacy_state(&from, &to).expect_err("occupied");
        assert_eq!(err.kind(), io::ErrorKind::AlreadyExists);
        assert!(from.join("state/a").exists());
        assert!(to.join("state/b").exists());
    }

    #[test]
    fn import_refuses_overlapping_directories() {
        let dir = tempdir();
        let from = dir.path().join("zebra");
        write_file(&from.join("state/a"), 1);
        let err = import_legacy_state(&from, &from.join("state")).expect_err("overlap");
        assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
    }

    #[cfg(unix)]
    #[test]
    fn symlinked_data_dirs_are_compared_resolved() {
        let dir = tempdir();
        let cache = dir.path().join("cache/zebra");
        write_file(&cache.join("state/v27/mainnet/000001.sst"), 3);
        let locations = LegacyLocations {
            exe_dir: None,
            zebra_cache_dir: Some(cache.clone()),
        };

        // The chosen data dir is a link to the zebra cache dir: its state is
        // the launcher's own, not a leftover, and cannot be imported.
        let link = dir.path().join("link");
        std::os::unix::fs::symlink(&cache, &link).expect("symlink");
        assert_eq!(detect_legacy(&locations, &link.join("state")), None);
        let err = import_legacy_state(&cache, &link).expect_err("same directory");
        assert_eq!(err.kind(), io::ErrorKind::InvalidInput);

        // A data dir reached through a link into the state itself would move
        // the state into itself.
        let into_state = dir.path().join("into-state");
        std::os::unix::fs::symlink(cache.join("state"), &into_state).expect("symlink");
        let to = into_state.join("sub");
        assert_eq!(detect_legacy(&locations, &to.join("state")), None);
        let err = import_legacy_state(&cache, &to).expect_err("into itself");
        assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
        assert!(cache.join("state/v27/mainnet/000001.sst").is_file());
        assert!(!cache.join("state/sub").exists());

        // A real directory elsewhere still sees the leftover.
        let found = detect_legacy(&locations, &dir.path().join("data/state")).expect("found");
        assert_eq!(found.state_dir, Some(cache.display().to_string()));
    }

    #[test]
    fn import_across_volumes_asks_for_a_manual_move() {
        let dir = tempdir();
        let from = dir.path().join("cache/zebra");
        let to = dir.path().join("data/state");
        write_file(&from.join("state/a"), 1);
        let err = import_with(&from, &to, |_, _| {
            Err(io::Error::from(io::ErrorKind::CrossesDevices))
        })
        .expect_err("cross-device");
        assert_eq!(err.kind(), io::ErrorKind::CrossesDevices);
        let message = err.to_string();
        assert!(message.contains("move the folder by hand"), "{message}");
        assert!(message.contains(&to.join("state").display().to_string()));
        assert!(
            from.join("state/a").exists(),
            "nothing was copied or removed"
        );

        #[cfg(unix)]
        let raw = io::Error::from_raw_os_error(nix::errno::Errno::EXDEV as i32);
        #[cfg(windows)]
        let raw = io::Error::from_raw_os_error(
            windows_sys::Win32::Foundation::ERROR_NOT_SAME_DEVICE as i32,
        );
        assert!(is_cross_device(&raw));
        assert!(!is_cross_device(&io::Error::from(
            io::ErrorKind::PermissionDenied
        )));
    }

    #[test]
    fn delete_removes_only_the_state() {
        let dir = tempdir();
        let paths = Paths::under(dir.path());
        paths.ensure_dirs().expect("dirs");
        let state = paths.default_state_dir();
        write_file(&state.join("state/v27/mainnet/a.sst"), 40);
        write_file(&state.join("b"), 2);
        write_file(&paths.zebrad_config(), 1);
        write_file(&paths.rpc_cookie_file(), 1);
        write_file(&paths.zebrad_log(), 1);

        let freed = delete_node_data(&state, &[paths.data_dir()]).expect("delete");
        assert_eq!(freed, 42);
        assert!(!state.exists());
        assert!(paths.zebrad_config().exists());
        assert!(paths.rpc_cookie_file().exists());
        assert!(paths.zebrad_log().exists());
        assert!(paths.data_dir().is_dir());
        // Already gone.
        assert_eq!(
            delete_node_data(&state, &[paths.data_dir()]).expect("again"),
            0
        );
    }

    #[test]
    fn delete_refuses_outside_the_allowed_roots() {
        let dir = tempdir();
        let root = dir.path().join("data");
        let chosen = dir.path().join("big-disk");
        let elsewhere = dir.path().join("elsewhere/state");
        for state in [root.join("state"), chosen.join("state"), elsewhere.clone()] {
            write_file(&state.join("x"), 1);
        }
        let roots = [root.as_path(), chosen.as_path()];

        let err = delete_node_data(&elsewhere, &roots).expect_err("outside");
        assert_eq!(err.kind(), io::ErrorKind::PermissionDenied);
        assert!(elsewhere.exists());

        // A root itself, or a directory not named `state`, is refused too.
        let err = delete_node_data(&root, &[dir.path()]).expect_err("not a state dir");
        assert_eq!(err.kind(), io::ErrorKind::PermissionDenied);
        write_file(&chosen.join("logs/x"), 1);
        let err = delete_node_data(&chosen.join("logs"), &roots).expect_err("not state");
        assert_eq!(err.kind(), io::ErrorKind::PermissionDenied);
        // `..` does not escape the check.
        let sneaky = root.join("..").join("elsewhere").join("state");
        let err = delete_node_data(&sneaky, &roots).expect_err("escapes");
        assert_eq!(err.kind(), io::ErrorKind::PermissionDenied);
        assert!(elsewhere.exists());

        assert_eq!(
            delete_node_data(&chosen.join("state"), &roots).expect("chosen"),
            1
        );
        assert!(!chosen.join("state").exists());
        assert!(root.join("state").exists());
    }
}
