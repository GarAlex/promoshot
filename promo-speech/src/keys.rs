//! Where the person's key comes from. The OS keyring first — macOS
//! Keychain, the Secret Service on Linux (GNOME Keyring, KWallet), the
//! Credential Manager on Windows — under service `promoshot`, one entry
//! per provider; then the environment, for a container or a CI box with
//! no keyring. Registering is `promoshot-mcp key set <provider>`, which
//! reads the key from stdin so it never lands in a shell history or a
//! config file. Nothing here logs a key, and no tool takes one as an
//! argument: an agent never sees it.

/// The keyring service every entry lives under.
pub const SERVICE: &str = "promoshot";

/// The providers, in the order the docs list them.
pub const PROVIDERS: [&str; 3] = ["openai", "elevenlabs", "google"];

/// The environment variable each provider's key may also come from.
pub const ENV_VARS: [(&str, &str); 3] = [
    ("openai", "OPENAI_API_KEY"),
    ("elevenlabs", "ELEVENLABS_API_KEY"),
    ("google", "GOOGLE_API_KEY"),
];

pub fn env_var(provider: &str) -> Option<&'static str> {
    ENV_VARS
        .iter()
        .find(|(p, _)| *p == provider)
        .map(|(_, v)| *v)
}

/// Where a key was found.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeySource {
    Keyring,
    Environment,
}

/// Something that answers a provider's key, and says where it got it.
pub trait KeyStore {
    fn key(&self, provider: &str) -> Option<(String, KeySource)>;
}

/// The keyring, then the environment.
pub struct SystemKeys;

impl KeyStore for SystemKeys {
    fn key(&self, provider: &str) -> Option<(String, KeySource)> {
        if let Ok(Some(key)) = keyring_get(provider) {
            return Some((key, KeySource::Keyring));
        }
        let var = env_var(provider)?;
        std::env::var(var)
            .ok()
            .map(|k| k.trim().to_string())
            .filter(|k| !k.is_empty())
            .map(|k| (k, KeySource::Environment))
    }
}

/// A fixed table — tests, and hosts that resolve keys themselves.
pub struct FixedKeys(pub Vec<(String, String)>);

impl KeyStore for FixedKeys {
    fn key(&self, provider: &str) -> Option<(String, KeySource)> {
        self.0
            .iter()
            .find(|(p, _)| p == provider)
            .map(|(_, k)| (k.clone(), KeySource::Environment))
    }
}

fn entry(provider: &str) -> Result<keyring::Entry, String> {
    if !PROVIDERS.contains(&provider) {
        return Err(format!(
            "provider `{provider}` — openai, elevenlabs or google"
        ));
    }
    keyring::Entry::new(SERVICE, provider).map_err(|e| format!("keyring: {e}"))
}

/// The keyring's answer for a provider: Ok(None) when there is no entry
/// (or no keyring on this machine), Err when the keyring refused.
pub fn keyring_get(provider: &str) -> Result<Option<String>, String> {
    match entry(provider)?.get_password() {
        Ok(key) => Ok(Some(key)),
        Err(keyring::Error::NoEntry) => Ok(None),
        // No usable store here (a container, a headless box without a
        // Secret Service): not an error to READ through — the environment
        // is next.
        Err(keyring::Error::NoStorageAccess(_)) | Err(keyring::Error::PlatformFailure(_)) => {
            Ok(None)
        }
        Err(e) => Err(format!("keyring: {e}")),
    }
}

/// Stores a provider's key in the keyring. Trimmed: a pasted key arrives
/// with the copy button's trailing newline more often than not, and a
/// header value with whitespace in it is a 401 that looks exactly like a
/// wrong key.
pub fn keyring_set(provider: &str, key: &str) -> Result<(), String> {
    let key = key.trim();
    if key.is_empty() {
        return Err("an empty key was not stored".into());
    }
    entry(provider)?
        .set_password(key)
        .map_err(|e| format!("keyring: {e}"))
}

/// Removes a provider's key from the keyring; Ok(false) when there was none.
pub fn keyring_remove(provider: &str) -> Result<bool, String> {
    match entry(provider)?.delete_credential() {
        Ok(()) => Ok(true),
        Err(keyring::Error::NoEntry) => Ok(false),
        Err(e) => Err(format!("keyring: {e}")),
    }
}

/// What `key status` reports, without ever printing the key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KeyStatus {
    /// Stored in the OS keyring.
    Keyring,
    /// Only in the environment.
    Environment(&'static str),
    /// Nowhere.
    Missing,
}

pub fn status(provider: &str) -> Result<KeyStatus, String> {
    if keyring_get(provider)?.is_some() {
        return Ok(KeyStatus::Keyring);
    }
    let var = env_var(provider).ok_or_else(|| format!("provider `{provider}`"))?;
    if std::env::var(var)
        .map(|v| !v.trim().is_empty())
        .unwrap_or(false)
    {
        return Ok(KeyStatus::Environment(var));
    }
    Ok(KeyStatus::Missing)
}

/// The honest refusal: how to register a key, and what to do without one.
pub fn missing_key_message(provider: &str) -> String {
    let var = env_var(provider).unwrap_or("OPENAI_API_KEY");
    format!(
        "no key for {provider}: the person registers one with `promoshot-mcp key set {provider}` \
         (stored in the OS keyring), or sets {var} in the server's environment. Without a key \
         an agent cannot narrate — record a voice file into Resources/ and reference it instead."
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_fixed_table_and_the_environment_answer_and_the_unknown_is_refused() {
        let keys = FixedKeys(vec![("openai".into(), "sk-test".into())]);
        assert_eq!(
            keys.key("openai"),
            Some(("sk-test".into(), KeySource::Environment))
        );
        assert_eq!(keys.key("google"), None);
        assert!(entry("nope").is_err());
        assert_eq!(env_var("elevenlabs"), Some("ELEVENLABS_API_KEY"));
        assert!(missing_key_message("google").contains("key set google"));
        assert!(missing_key_message("google").contains("GOOGLE_API_KEY"));
        assert!(
            keyring_set("openai", "   ").is_err(),
            "an empty key is never stored"
        );
    }

    /// The keyring round trip, on a machine that has one. A box without a
    /// usable store (CI, a container) reads as "no entry" rather than an
    /// error, so the environment stays reachable behind it.
    #[test]
    fn the_keyring_round_trips_or_reads_as_absent() {
        let provider = "elevenlabs";
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
