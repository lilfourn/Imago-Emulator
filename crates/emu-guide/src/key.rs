//! The OpenRouter key Guide uses. Sign-in writes it to `auth.json`
//! (`{"key": "sk-or-…"}`) in a folder of its own, readable by its owner
//! alone, as Imago keeps its key. `OPENROUTER_API_KEY` in the environment
//! overrides the file, for testing.
//!
//! A file rather than the Keychain, as in Imago: the app is signed ad hoc,
//! so a Keychain item's access list would name a code signature that
//! changes with every build, and macOS would ask again after each one.

use std::fmt;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use serde_json::Value;

/// The variable whose key, when set, Guide uses in place of the file's.
pub const KEY_ENV: &str = "OPENROUTER_API_KEY";

/// The file the key is kept in, inside the store's folder.
const FILE: &str = "auth.json";

/// Where a key came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Source {
    /// The key file sign-in wrote.
    File,
    /// [`KEY_ENV`], which wins over the file.
    Environment,
}

/// An OpenRouter key. Its `Debug` shows only its last characters.
#[derive(Clone, PartialEq, Eq)]
pub struct Key {
    secret: String,
    source: Source,
}

impl Key {
    /// The key itself, for a request's `Authorization` header only.
    pub fn secret(&self) -> &str {
        &self.secret
    }

    /// Where the key came from.
    pub fn source(&self) -> Source {
        self.source
    }

    /// The key's last four characters, enough to tell keys apart.
    pub fn tail(&self) -> &str {
        let start = self
            .secret
            .char_indices()
            .rev()
            .nth(3)
            .map_or(0, |(at, _)| at);
        &self.secret[start..]
    }
}

impl fmt::Debug for Key {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Key(…{}, {:?})", self.tail(), self.source)
    }
}

/// The folder the key file lives in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeyStore {
    folder: PathBuf,
}

impl KeyStore {
    /// A store keeping its key in `folder`.
    pub fn new(folder: impl Into<PathBuf>) -> Self {
        Self {
            folder: folder.into(),
        }
    }

    /// The key file.
    pub fn file(&self) -> PathBuf {
        self.folder.join(FILE)
    }

    /// The key to use: [`KEY_ENV`]'s if set, else the file's, if there is
    /// one.
    pub fn load(&self) -> Option<Key> {
        self.load_with(std::env::var(KEY_ENV).ok())
    }

    /// [`load`](Self::load), with `environment` standing for [`KEY_ENV`]'s
    /// value.
    pub fn load_with(&self, environment: Option<String>) -> Option<Key> {
        if let Some(secret) = environment.filter(|secret| !secret.trim().is_empty()) {
            return Some(Key {
                secret: secret.trim().to_owned(),
                source: Source::Environment,
            });
        }
        let text = fs::read_to_string(self.file()).ok()?;
        let auth: Value = serde_json::from_str(&text).ok()?;
        let secret = auth.get("key")?.as_str()?.trim();
        (!secret.is_empty()).then(|| Key {
            secret: secret.to_owned(),
            source: Source::File,
        })
    }

    /// Keeps `secret` as the key, replacing any before it, in a file only
    /// its owner can read, written whole or not at all.
    ///
    /// # Errors
    ///
    /// If the folder or file cannot be made or written.
    pub fn save(&self, secret: &str) -> io::Result<()> {
        private_folder(&self.folder)?;
        let json = serde_json::json!({ "key": secret }).to_string();
        write_private(&self.file(), json.as_bytes())
    }

    /// Forgets the key: removes its file, if there is one. A key from
    /// [`KEY_ENV`] stays.
    ///
    /// # Errors
    ///
    /// If the file is there but cannot be removed.
    pub fn forget(&self) -> io::Result<()> {
        match fs::remove_file(self.file()) {
            Err(error) if error.kind() != io::ErrorKind::NotFound => Err(error),
            _ => Ok(()),
        }
    }
}

/// Makes `folder` and its parents, the folder itself readable by its owner
/// alone. On Windows it sets nothing: the folder is in the user's own
/// `%LOCALAPPDATA%`, which only they (and administrators) can read, and
/// what is made inside it inherits that.
fn private_folder(folder: &Path) -> io::Result<()> {
    fs::create_dir_all(folder)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(folder, fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}

/// Writes `bytes` to `path` through a temporary file beside it, created
/// readable by its owner alone (on Windows, by the user alone through the
/// folder's own access list, as [`private_folder`] says), then renamed over
/// `path`.
fn write_private(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let mut temporary = path.as_os_str().to_owned();
    temporary.push(".tmp");
    let temporary = PathBuf::from(temporary);
    // A leftover from an interrupted write keeps its old permissions when
    // opened again, so it goes first.
    match fs::remove_file(&temporary) {
        Err(error) if error.kind() != io::ErrorKind::NotFound => return Err(error),
        _ => {}
    }
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file: File = options.open(&temporary)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    drop(file);
    fs::rename(&temporary, path)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A folder of its own for one test, emptied first.
    fn folder(name: &str) -> PathBuf {
        let folder = std::env::temp_dir()
            .join(format!("emu-guide-key-{}", std::process::id()))
            .join(name);
        fs::remove_dir_all(&folder).ok();
        folder
    }

    #[test]
    fn a_saved_key_loads_back_and_only_its_owner_can_read_it() {
        let store = KeyStore::new(folder("saved").join("OpenRouter"));
        assert_eq!(store.load_with(None), None);
        store.save("sk-or-v1-first").unwrap();
        store.save("sk-or-v1-second1234").unwrap();
        let key = store.load_with(None).unwrap();
        assert_eq!(key.secret(), "sk-or-v1-second1234");
        assert_eq!(key.source(), Source::File);
        assert_eq!(key.tail(), "1234");
        assert_eq!(format!("{key:?}"), "Key(…1234, File)");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = |path: &Path| fs::metadata(path).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode(&store.file()), 0o600);
            assert_eq!(mode(&store.folder), 0o700);
        }
        assert!(!store.folder.join("auth.json.tmp").exists());
    }

    #[test]
    fn the_environment_wins_and_forgetting_leaves_it() {
        let store = KeyStore::new(folder("environment"));
        store.save("sk-or-v1-file").unwrap();
        let key = store
            .load_with(Some(" sk-or-v1-env \n".to_owned()))
            .unwrap();
        assert_eq!(key.secret(), "sk-or-v1-env");
        assert_eq!(key.source(), Source::Environment);
        assert_eq!(
            store.load_with(Some("  ".to_owned())).unwrap().source(),
            Source::File
        );
        store.forget().unwrap();
        store.forget().unwrap();
        assert_eq!(store.load_with(None), None);
        assert!(store.load_with(Some("sk-or-v1-env".to_owned())).is_some());
    }

    #[test]
    fn a_broken_or_empty_file_is_no_key() {
        let store = KeyStore::new(folder("broken"));
        private_folder(&store.folder).unwrap();
        fs::write(store.file(), "{not json").unwrap();
        assert_eq!(store.load_with(None), None);
        fs::write(store.file(), r#"{"key": " "}"#).unwrap();
        assert_eq!(store.load_with(None), None);
    }

    #[test]
    fn short_keys_show_whole() {
        let key = Key {
            secret: "abc".to_owned(),
            source: Source::File,
        };
        assert_eq!(key.tail(), "abc");
    }
}
