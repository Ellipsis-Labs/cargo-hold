use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::{LazyLock, Mutex, MutexGuard};
use std::{env, fs};

use tempfile::TempDir;

/// Global mutex to ensure only one test manipulates HOME/CARGO_HOME at a time.
static HOME_MUTEX: LazyLock<Mutex<()>> = LazyLock::new(|| Mutex::new(()));

/// RAII guard that redirects HOME and CARGO_HOME to a temporary directory.
///
/// While the guard is alive, filesystem operations that resolve the user's home
/// directory are confined to the sandbox directory, ensuring tests cannot touch
/// the developer's real cargo caches or credentials.
pub struct TempHomeGuard {
    _lock: MutexGuard<'static, ()>,
    temp_home: TempDir,
    prev_home: Option<OsString>,
    prev_cargo_home: Option<OsString>,
    #[cfg(windows)]
    prev_userprofile: Option<OsString>,
    #[cfg(windows)]
    prev_homedrive: Option<OsString>,
    #[cfg(windows)]
    prev_homepath: Option<OsString>,
}

impl TempHomeGuard {
    /// Create a new guard with a fresh temporary home directory.
    pub fn new() -> Self {
        let lock = HOME_MUTEX.lock().expect("temporary home mutex poisoned");

        let temp_home = TempDir::new().expect("failed to create temporary HOME");
        let home_path = temp_home.path();
        let cargo_home = home_path.join(".cargo");
        fs::create_dir_all(&cargo_home).expect("failed to create temporary CARGO_HOME");

        let prev_home = env::var_os("HOME");
        let prev_cargo_home = env::var_os("CARGO_HOME");
        #[cfg(windows)]
        let prev_userprofile = env::var_os("USERPROFILE");
        #[cfg(windows)]
        let prev_homedrive = env::var_os("HOMEDRIVE");
        #[cfg(windows)]
        let prev_homepath = env::var_os("HOMEPATH");

        // SAFETY: we hold HOME_MUTEX, ensuring no other thread mutates the
        // environment while we redirect HOME/CARGO_HOME for the test.
        unsafe {
            env::set_var("HOME", home_path);
            env::set_var("CARGO_HOME", &cargo_home);
            #[cfg(windows)]
            {
                let (drive, homepath) = split_windows_home_vars(home_path);
                env::set_var("USERPROFILE", home_path);
                env::set_var("HOMEDRIVE", drive);
                env::set_var("HOMEPATH", homepath);
            }
        }

        Self {
            _lock: lock,
            temp_home,
            prev_home,
            prev_cargo_home,
            #[cfg(windows)]
            prev_userprofile,
            #[cfg(windows)]
            prev_homedrive,
            #[cfg(windows)]
            prev_homepath,
        }
    }

    /// Path to the temporary HOME directory.
    pub fn home(&self) -> &Path {
        self.temp_home.path()
    }

    /// Path to the temporary CARGO_HOME directory.
    pub fn cargo_home(&self) -> PathBuf {
        self.temp_home.path().join(".cargo")
    }
}

impl Drop for TempHomeGuard {
    fn drop(&mut self) {
        // SAFETY: guarded by HOME_MUTEX; we restore the environment to its
        // previous state before releasing the lock.
        unsafe {
            if let Some(prev) = self.prev_home.as_ref() {
                env::set_var("HOME", prev);
            } else {
                env::remove_var("HOME");
            }

            if let Some(prev) = self.prev_cargo_home.as_ref() {
                env::set_var("CARGO_HOME", prev);
            } else {
                env::remove_var("CARGO_HOME");
            }
            #[cfg(windows)]
            {
                if let Some(prev) = self.prev_userprofile.as_ref() {
                    env::set_var("USERPROFILE", prev);
                } else {
                    env::remove_var("USERPROFILE");
                }

                if let Some(prev) = self.prev_homedrive.as_ref() {
                    env::set_var("HOMEDRIVE", prev);
                } else {
                    env::remove_var("HOMEDRIVE");
                }

                if let Some(prev) = self.prev_homepath.as_ref() {
                    env::set_var("HOMEPATH", prev);
                } else {
                    env::remove_var("HOMEPATH");
                }
            }
        }
        // temp_home drops here, cleaning up the directory
    }
}

#[cfg(windows)]
fn split_windows_home_vars(home: &Path) -> (OsString, OsString) {
    use std::path::{Component, Path};

    let mut components = home.components();
    if let Some(Component::Prefix(prefix)) = components.next() {
        let drive = prefix.as_os_str().to_os_string();
        let rest_path = home
            .strip_prefix(prefix.as_os_str())
            .unwrap_or_else(|_| Path::new(""));
        let rest = normalize_homepath(rest_path);
        (drive, rest)
    } else {
        (OsString::new(), normalize_homepath(home))
    }
}

#[cfg(windows)]
fn normalize_homepath(path: &Path) -> OsString {
    use std::borrow::Cow;

    let raw = path.as_os_str();
    if raw.is_empty() {
        return OsString::from("\\");
    }

    let mut text = Cow::Owned(path.display().to_string());
    if !text.starts_with(['\\', '/']) {
        let mut prefixed = String::from("\\");
        prefixed.push_str(&text);
        text = Cow::Owned(prefixed);
    }

    let normalized = text.replace('/', "\\");
    OsString::from(normalized)
}
