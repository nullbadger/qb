use {
    crate::k8s::FavoriteType,
    anyhow::{
        Context,
        Result,
    },
    serde::{
        Deserialize,
        Serialize,
    },
    std::{
        collections::HashMap,
        path::PathBuf,
    },
};

// ---------------------------------------------------------------------------
// Config types
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct QbConfig {
    pub version: String,
    pub active_profile: String,
    #[serde(default)]
    pub profiles: HashMap<String, Profile>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Profile {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kubeconfig: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context: Option<String>,
    #[serde(default)]
    pub favorites: Vec<FavoriteEntry>,
    #[serde(default)]
    pub port_forwards: Vec<SavedPortForward>,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct FavoriteEntry {
    pub resource_type: FavoriteType,
    pub name: String,
    pub namespace: String,
    pub context: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SavedPfResource {
    #[serde(rename = "resource_type")]
    pub r#type: String,
    #[serde(rename = "resource_name")]
    pub name: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SavedPfPorts {
    pub local_port: u16,
    pub remote_port: u16,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SavedPfTarget {
    pub target_type: String,
    pub selector: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SavedPortForward {
    #[serde(flatten)]
    pub resource: SavedPfResource,
    pub namespace: String,
    pub context: String,
    #[serde(flatten)]
    pub port: SavedPfPorts,
    #[serde(flatten)]
    pub target: SavedPfTarget,
    #[serde(default)]
    pub paused: bool,
}

// ---------------------------------------------------------------------------
// Config path
// ---------------------------------------------------------------------------

impl QbConfig {
    fn path() -> Result<PathBuf> {
        let home = dirs::home_dir().context("Could not determine home directory")?;
        Ok(home.join(".config").join("qb").join("config.yaml"))
    }
}

// ---------------------------------------------------------------------------
// Version checking (secenv-style)
// ---------------------------------------------------------------------------

/// Validate that a config file version is compatible with the current CLI
/// version.
///
/// Rules:
/// - CLI version `0.0.0` (dev build) accepts any config.
/// - CLI `< 1.0.0`: config minor version must match CLI minor version.
/// - CLI `>= 1.0.0`: config major version must match CLI major version.
/// - Config version newer than CLI → error.
fn validate_version(config_version: &str) -> Result<()> {
    let cli_version = semver::Version::parse(env!("CARGO_PKG_VERSION")).context("Failed to parse CLI version")?;
    versions_compatible(&cli_version, config_version)
}

fn versions_compatible(cli_version: &semver::Version, config_version: &str) -> Result<()> {
    // Dev build: accept anything
    if cli_version.major == 0 && cli_version.minor == 0 && cli_version.patch == 0 {
        return Ok(());
    }

    let cfg_version = semver::Version::parse(config_version)
        .with_context(|| format!("Invalid version in config: '{}'", config_version))?;

    if cli_version.major == 0 {
        // Pre-1.0: minor must match
        if cfg_version.minor != cli_version.minor {
            anyhow::bail!(
                "Config version {} is incompatible with CLI version {} (minor version mismatch, pre-1.0).",
                cfg_version,
                cli_version
            );
        }
    } else if cfg_version.major != cli_version.major {
        anyhow::bail!(
            "Config version {} is incompatible with CLI version {} (major version mismatch).",
            cfg_version,
            cli_version
        );
    }

    if cfg_version > *cli_version {
        anyhow::bail!(
            "Config version {} is newer than CLI version {}. Please upgrade qb.",
            cfg_version,
            cli_version
        );
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// Load / Save
// ---------------------------------------------------------------------------

impl QbConfig {
    /// Create a default config with a single "default" profile.
    pub fn default_config() -> Self {
        let mut profiles = HashMap::new();
        profiles.insert("default".to_string(), Profile::default());
        let mut config = Self {
            version: env!("CARGO_PKG_VERSION").to_string(),
            active_profile: "default".to_string(),
            profiles,
        };
        config.normalize();
        config
    }

    /// The active profile name always has an entry after this runs.
    fn normalize(&mut self) {
        if self.active_profile.is_empty() {
            self.active_profile = "default".to_string();
        }
        self.profiles.entry(self.active_profile.clone()).or_default();
    }

    /// Load config from `~/.config/qb/config.yaml`. Creates a default config
    /// if the file doesn't exist.
    pub fn load() -> Result<Self> {
        let path = Self::path()?;

        if !path.exists() {
            let mut config = Self::default_config();
            config.save()?;
            return Ok(config);
        }

        let contents =
            std::fs::read_to_string(&path).with_context(|| format!("Failed to read config: {}", path.display()))?;

        let mut config: QbConfig =
            serde_yaml::from_str(&contents).with_context(|| format!("Failed to parse config: {}", path.display()))?;

        validate_version(&config.version)?;
        config.normalize();

        Ok(config)
    }

    /// Save config to `~/.config/qb/config.yaml`.
    pub fn save(&mut self) -> Result<()> {
        self.normalize();
        let path = Self::path()?;

        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("Failed to create config directory: {}", parent.display()))?;
        }

        let yaml = serde_yaml::to_string(self).context("Failed to serialize config")?;
        std::fs::write(&path, yaml).with_context(|| format!("Failed to write config: {}", path.display()))?;

        Ok(())
    }

    /// Active profile. `load`, `default_config`, and `save` keep the name
    /// mapped.
    pub fn active_profile(&self) -> &Profile {
        self.profiles
            .get(&self.active_profile)
            .expect("active profile is inserted by QbConfig::normalize")
    }

    /// Switch to an existing profile. Returns false if `name` is not stored.
    pub fn activate(&mut self, name: &str) -> bool {
        if !self.profiles.contains_key(name) {
            return false;
        }
        self.active_profile = name.to_string();
        true
    }

    /// Copy the active profile under `name` and make it active.
    pub fn save_as(&mut self, name: String) {
        let profile = self.active_profile().clone();
        self.profiles.insert(name.clone(), profile);
        self.active_profile = name;
    }

    /// Get a mutable reference to the active profile, creating it if needed.
    pub fn active_profile_mut(&mut self) -> &mut Profile {
        let name = self.active_profile.clone();
        self.profiles.entry(name).or_default()
    }

    /// List all profile names.
    pub fn profile_names(&self) -> Vec<&str> {
        self.profiles.keys().map(|s| s.as_str()).collect()
    }
}

impl Profile {
    /// Check if a resource is in the favorites list.
    pub fn is_favorite(&self, resource_type: &FavoriteType, name: &str, namespace: &str, context: &str) -> bool {
        self.favorites.iter().any(|f| {
            &f.resource_type == resource_type && f.name == name && f.namespace == namespace && f.context == context
        })
    }

    /// Toggle a resource as a favorite. Returns true if added, false if
    /// removed.
    pub fn toggle_favorite(
        &mut self,
        resource_type: FavoriteType,
        name: String,
        namespace: String,
        context: String,
    ) -> bool {
        let entry = FavoriteEntry {
            resource_type,
            name,
            namespace,
            context,
        };
        if let Some(pos) = self.favorites.iter().position(|f| f == &entry) {
            self.favorites.remove(pos);
            false
        } else {
            self.favorites.push(entry);
            true
        }
    }

    /// Add a saved port forward.
    pub fn add_port_forward(&mut self, pf: SavedPortForward) {
        if !self.port_forwards.iter().any(|existing| existing.same_endpoint(&pf)) {
            self.port_forwards.push(pf);
        }
    }

    /// Remove a saved port forward by matching key fields.
    pub fn remove_port_forward(&mut self, resource_name: &str, namespace: &str, context: &str, local_port: u16) {
        self.port_forwards
            .retain(|pf| !pf.matches(resource_name, namespace, context, local_port));
    }
}

impl SavedPortForward {
    pub fn matches(&self, resource_name: &str, namespace: &str, context: &str, local_port: u16) -> bool {
        self.resource.name == resource_name
            && self.namespace == namespace
            && self.context == context
            && self.port.local_port == local_port
    }

    fn same_endpoint(&self, other: &Self) -> bool {
        self.resource.name == other.resource.name
            && self.namespace == other.namespace
            && self.context == other.context
            && self.port.local_port == other.port.local_port
            && self.port.remote_port == other.port.remote_port
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ver(s: &str) -> semver::Version {
        semver::Version::parse(s).unwrap()
    }

    #[test]
    fn version_rules() {
        assert!(versions_compatible(&ver("0.0.0"), "9.9.9").is_ok());
        assert!(versions_compatible(&ver("0.2.4"), "0.2.0").is_ok());
        assert!(versions_compatible(&ver("0.2.0"), "0.2.4").is_err());
        assert!(versions_compatible(&ver("0.2.0"), "0.3.0").is_err());
        assert!(versions_compatible(&ver("1.4.0"), "1.2.0").is_ok());
        assert!(versions_compatible(&ver("1.4.0"), "2.0.0").is_err());
        assert!(versions_compatible(&ver("1.4.0"), "1.5.0").is_err());
        assert!(versions_compatible(&ver("1.4.0"), "not-a-version").is_err());
    }

    #[test]
    fn saved_port_forward_keeps_flat_yaml_fields() {
        let pf = SavedPortForward {
            resource: SavedPfResource {
                r#type: "deployment".into(),
                name: "api".into(),
            },
            namespace: "prod".into(),
            context: "kind".into(),
            port: SavedPfPorts {
                local_port: 8080,
                remote_port: 80,
            },
            target: SavedPfTarget {
                target_type: "label_selector".into(),
                selector: "app=api".into(),
            },
            paused: false,
        };
        let yaml = serde_yaml::to_string(&pf).unwrap();
        assert!(yaml.contains("resource_type: deployment"));
        assert!(yaml.contains("resource_name: api"));
        assert!(yaml.contains("local_port: 8080"));
        assert!(yaml.contains("target_type: label_selector"));
        assert!(!yaml.contains("resource:"));
        let back: SavedPortForward = serde_yaml::from_str(&yaml).unwrap();
        assert!(back.matches("api", "prod", "kind", 8080));
    }

    #[test]
    fn normalize_inserts_missing_active_profile() {
        let mut config = QbConfig {
            version: "0.0.0".into(),
            active_profile: "missing".into(),
            profiles: HashMap::new(),
        };
        config.normalize();
        assert!(config.profiles.contains_key("missing"));
        assert!(config.active_profile().favorites.is_empty());
    }
}
