//! Where the person's key comes from. The OS keyring first — macOS
//! Keychain, the Secret Service on Linux (GNOME Keyring, KWallet), the
//! Credential Manager on Windows — under service `promoshot`, one entry
//! per provider; registered with `promoshot-mcp key set <provider>`,
//! which reads the key from stdin so it never lands in a shell history or
//! a config file. Where no keyring exists — a container, a CI runner —
//! the key is read from a SECRETS FILE: the path in
//! `OPENAI_API_KEY_FILE` (`ELEVENLABS_…`, `GOOGLE_…`), else Docker's own
//! convention, `/run/secrets/OPENAI_API_KEY`. That is how Docker,
//! Kubernetes and most CI systems hand a secret over: a mode-0400 file,
//! never an environment variable that `docker inspect` and every
//! same-user process can read. Nothing here stores or logs a key, and no
//! tool takes one as an argument: an agent never sees it.

use std::path::{Path, PathBuf};

/// The keyring service every entry lives under.
pub const SERVICE: &str = "promoshot";

/// The providers, in the order the docs list them.
pub const PROVIDERS: [&str; 3] = ["openai", "elevenlabs", "google"];

/// Per provider: the variable that may name a secrets file, and the
/// Docker-convention path read when it does not.
pub const SECRETS: [(&str, &str, &str); 3] = [
    (
        "openai",
        "OPENAI_API_KEY_FILE",
        "/run/secrets/OPENAI_API_KEY",
    ),
    (
        "elevenlabs",
        "ELEVENLABS_API_KEY_FILE",
        "/run/secrets/ELEVENLABS_API_KEY",
    ),
    (
        "google",
        "GOOGLE_API_KEY_FILE",
        "/run/secrets/GOOGLE_API_KEY",
    ),
];

/// The secrets file a provider's key would be read from: the path named
/// by its `*_API_KEY_FILE` variable, else the Docker default.
pub fn secrets_path(provider: &str) -> Option<PathBuf> {
    let (_, var, default) = SECRETS.iter().find(|(p, _, _)| *p == provider)?;
    Some(
        std::env::var(var)
            .ok()
            .filter(|p| !p.trim().is_empty())
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from(default)),
    )
}

/// Where a key was found.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeySource {
    Keyring,
    SecretsFile,
    /// Handed in by the host (a fixed table, the apps' Keychain).
    Given,
}

/// Something that answers a provider's key, and says where it got it.
pub trait KeyStore {
    fn key(&self, provider: &str) -> Option<(String, KeySource)>;
}

/// The keyring, then the secrets file.
pub struct SystemKeys;

impl KeyStore for SystemKeys {
    fn key(&self, provider: &str) -> Option<(String, KeySource)> {
        if let Ok(Some(key)) = keyring_get(provider) {
            return Some((key, KeySource::Keyring));
        }
        secrets_file_key(provider).map(|k| (k, KeySource::SecretsFile))
    }
}

/// The key in the provider's secrets file, trimmed; None when there is no
/// file, it is empty, or it cannot be read.
pub fn secrets_file_key(provider: &str) -> Option<String> {
    let path = secrets_path(provider)?;
    read_secret(&path)
}

fn read_secret(path: &Path) -> Option<String> {
    std::fs::read_to_string(path)
        .ok()
        .map(|k| k.trim().to_string())
        .filter(|k| !k.is_empty())
}

/// A fixed table — tests, and hosts that resolve keys themselves.
pub struct FixedKeys(pub Vec<(String, String)>);

impl KeyStore for FixedKeys {
    fn key(&self, provider: &str) -> Option<(String, KeySource)> {
        self.0
            .iter()
            .find(|(p, _)| p == provider)
            .map(|(_, k)| (k.clone(), KeySource::Given))
    }
}

fn known(provider: &str) -> Result<(), String> {
    if PROVIDERS.contains(&provider) {
        Ok(())
    } else {
        Err(format!(
            "provider `{provider}` — openai, elevenlabs or google"
        ))
    }
}

#[cfg(feature = "keyring")]
fn entry(provider: &str) -> Result<keyring::Entry, String> {
    known(provider)?;
    keyring::Entry::new(SERVICE, provider).map_err(|e| format!("keyring: {e}"))
}

/// The keyring's answer for a provider: Ok(None) when there is no entry
/// (or no keyring on this machine, or this build carries none), Err when
/// the keyring refused.
pub fn keyring_get(provider: &str) -> Result<Option<String>, String> {
    known(provider)?;
    #[cfg(feature = "keyring")]
    {
        match entry(provider)?.get_password() {
            Ok(key) => Ok(Some(key)),
            Err(keyring::Error::NoEntry) => Ok(None),
            // No usable store here (a container, a headless box without a
            // Secret Service): not an error to READ through — the secrets
            // file is next.
            Err(keyring::Error::NoStorageAccess(_)) | Err(keyring::Error::PlatformFailure(_)) => {
                Ok(None)
            }
            Err(e) => Err(format!("keyring: {e}")),
        }
    }
    #[cfg(not(feature = "keyring"))]
    {
        Ok(None)
    }
}

/// Stores a provider's key in the keyring. Trimmed: a pasted key arrives
/// with the copy button's trailing newline more often than not, and a
/// header value with whitespace in it is a 401 that looks exactly like a
/// wrong key.
pub fn keyring_set(provider: &str, key: &str) -> Result<(), String> {
    known(provider)?;
    let key = key.trim();
    if key.is_empty() {
        return Err("an empty key was not stored".into());
    }
    #[cfg(feature = "keyring")]
    {
        entry(provider)?
            .set_password(key)
            .map_err(|e| format!("keyring: {e}"))
    }
    #[cfg(not(feature = "keyring"))]
    {
        Err("this build carries no keyring; mount the provider's secrets file instead".into())
    }
}

/// Removes a provider's key from the keyring; Ok(false) when there was none.
pub fn keyring_remove(provider: &str) -> Result<bool, String> {
    known(provider)?;
    #[cfg(feature = "keyring")]
    {
        match entry(provider)?.delete_credential() {
            Ok(()) => Ok(true),
            Err(keyring::Error::NoEntry) => Ok(false),
            Err(e) => Err(format!("keyring: {e}")),
        }
    }
    #[cfg(not(feature = "keyring"))]
    {
        Ok(false)
    }
}

/// What `key status` reports, without ever printing the key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KeyStatus {
    /// Stored in the OS keyring.
    Keyring,
    /// Read from a secrets file at this path.
    SecretsFile(PathBuf),
    /// Nowhere.
    Missing,
}

pub fn status(provider: &str) -> Result<KeyStatus, String> {
    if keyring_get(provider)?.is_some() {
        return Ok(KeyStatus::Keyring);
    }
    let path = secrets_path(provider).ok_or_else(|| format!("provider `{provider}`"))?;
    if read_secret(&path).is_some() {
        return Ok(KeyStatus::SecretsFile(path));
    }
    Ok(KeyStatus::Missing)
}

/// The honest refusal: how to register a key, and what to do without one.
pub fn missing_key_message(provider: &str) -> String {
    let (var, default) = SECRETS
        .iter()
        .find(|(p, _, _)| *p == provider)
        .map(|(_, v, d)| (*v, *d))
        .unwrap_or(("OPENAI_API_KEY_FILE", "/run/secrets/OPENAI_API_KEY"));
    format!(
        "no key for {provider}: the person registers one with `promoshot-mcp key set {provider}` \
         (stored in the OS keyring), or — where there is no keyring, a container — mounts it as a \
         secrets file at {default} (or the path in {var}). Without a key an agent cannot narrate — \
         record a voice file into Resources/ and reference it instead."
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_fixed_table_answers_and_the_unknown_is_refused() {
        let keys = FixedKeys(vec![("openai".into(), "sk-test".into())]);
        assert_eq!(
            keys.key("openai"),
            Some(("sk-test".into(), KeySource::Given))
        );
        assert_eq!(keys.key("google"), None);
        assert!(keyring_get("nope").is_err());
        assert!(secrets_path("nope").is_none());
        assert!(missing_key_message("google").contains("key set google"));
        assert!(missing_key_message("google").contains("/run/secrets/GOOGLE_API_KEY"));
        assert!(missing_key_message("google").contains("GOOGLE_API_KEY_FILE"));
        assert!(
            keyring_set("openai", "   ").is_err(),
            "an empty key is never stored"
        );
    }

    /// The secrets file: the path a variable names, else Docker's default;
    /// trimmed; an empty or missing file is no key. The variable here is
    /// the elevenlabs one alone, so no other test reads it.
    #[test]
    fn the_secrets_file_is_the_fallback_and_never_the_environment() {
        assert_eq!(
            secrets_path("openai").unwrap(),
            PathBuf::from("/run/secrets/OPENAI_API_KEY"),
            "Docker's convention when no variable names a path"
        );
        let dir = std::env::temp_dir().join(format!("promo-speech-secret-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("eleven");
        std::env::set_var("ELEVENLABS_API_KEY_FILE", &file);
        assert_eq!(secrets_file_key("elevenlabs"), None, "no file yet");
        std::fs::write(&file, "  el-secret\n").unwrap();
        assert_eq!(secrets_file_key("elevenlabs").as_deref(), Some("el-secret"));
        match status("elevenlabs").unwrap() {
            KeyStatus::Keyring => {} // a keyring entry on this machine outranks the file
            other => assert_eq!(other, KeyStatus::SecretsFile(file.clone())),
        }
        std::fs::write(&file, "\n").unwrap();
        assert_eq!(
            secrets_file_key("elevenlabs"),
            None,
            "an empty file is no key"
        );
        std::env::remove_var("ELEVENLABS_API_KEY_FILE");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(feature = "keyring")]
    /// The keyring round trip, on a machine that has one. A box without a
    /// usable store (CI, a container) reads as "no entry" rather than an
    /// error, so the secrets file stays reachable behind it.
    #[test]
    fn the_keyring_round_trips_or_reads_as_absent() {
        let provider = "google";
        match keyring_set(provider, "test-key-do-not-keep") {
            Ok(()) => {
                assert_eq!(
                    keyring_get(provider).unwrap().as_deref(),
                    Some("test-key-do-not-keep")
                );
                assert_eq!(status(provider).unwrap(), KeyStatus::Keyring);
                assert!(keyring_remove(provider).unwrap());
                assert_eq!(keyring_get(provider).unwrap(), None);
            }
            Err(e) => {
                eprintln!("no usable keyring here ({e}); reading must still answer None");
                assert_eq!(keyring_get(provider).unwrap(), None);
            }
        }
    }
}
