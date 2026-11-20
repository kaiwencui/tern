//! Runtime app identity derived from the macOS app bundle (stable `Tern` vs
//! preview `Tern Next`). Unbundled runs (e.g. `cargo run` or tests) fall back
//! to the stable identity.

use std::sync::OnceLock;

use objc2_foundation::{ns_string, NSBundle, NSString};

pub const NEXT_BUNDLE_ID: &str = "com.kaicui.tern.next";

const DEFAULT_APP_NAME: &str = "Tern";
const NEXT_APP_NAME: &str = "Tern Next";
const STABLE_STATE_DIR: &str = "tern";
const NEXT_STATE_DIR: &str = "tern-next";
const STABLE_TABBING_ID: &str = "com.kaicui.tern.terminal";
const NEXT_TABBING_ID: &str = "com.kaicui.tern.next.terminal";

#[derive(Clone, Debug, PartialEq, Eq)]
struct Identity {
    bundle_id: Option<String>,
    app_name: String,
    is_next: bool,
}

impl Identity {
    fn from_main_bundle() -> Self {
        let bundle = NSBundle::mainBundle();
        let bundle_id = bundle.bundleIdentifier().map(|id| id.to_string());
        let display_name = info_string(&bundle, ns_string!("CFBundleDisplayName"));
        let bundle_name = info_string(&bundle, ns_string!("CFBundleName"));
        Self::resolve(bundle_id, display_name, bundle_name)
    }

    fn resolve(
        bundle_id: Option<String>,
        display_name: Option<String>,
        bundle_name: Option<String>,
    ) -> Self {
        let is_next = bundle_id.as_deref() == Some(NEXT_BUNDLE_ID);
        let default_name = if is_next {
            NEXT_APP_NAME
        } else {
            DEFAULT_APP_NAME
        };
        let app_name = if bundle_id.is_some() {
            display_name
                .or(bundle_name)
                .map(|s| s.trim().to_owned())
                .filter(|s| !s.is_empty())
                .unwrap_or_else(|| default_name.to_owned())
        } else {
            DEFAULT_APP_NAME.to_owned()
        };
        Self {
            bundle_id,
            app_name,
            is_next,
        }
    }

    fn is_bundled(&self) -> bool {
        self.bundle_id.is_some()
    }

    fn state_dir_name(&self) -> &'static str {
        if self.is_next {
            NEXT_STATE_DIR
        } else {
            STABLE_STATE_DIR
        }
    }

    fn tabbing_id(&self) -> &'static str {
        if self.is_next {
            NEXT_TABBING_ID
        } else {
            STABLE_TABBING_ID
        }
    }
}

fn info_string(bundle: &NSBundle, key: &NSString) -> Option<String> {
    bundle
        .objectForInfoDictionaryKey(key)
        .and_then(|obj| obj.downcast::<NSString>().ok())
        .map(|s| s.to_string())
}

fn current() -> &'static Identity {
    static IDENTITY: OnceLock<Identity> = OnceLock::new();
    IDENTITY.get_or_init(Identity::from_main_bundle)
}

/// User-facing application name (`"Tern"` or `"Tern Next"`).
pub fn app_name() -> &'static str {
    &current().app_name
}

/// Whether this process is running from a bundled macOS `.app`.
pub fn is_bundled() -> bool {
    current().is_bundled()
}

/// Directory name under `$XDG_STATE_HOME` / `~/.local/state` for per-app state.
pub fn state_dir_name() -> &'static str {
    current().state_dir_name()
}

/// `NSWindow.tabbingIdentifier` shared by windows of this app variant.
pub fn tabbing_id() -> &'static str {
    current().tabbing_id()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unbundled_falls_back_to_stable_identity() {
        let id = Identity::resolve(None, Some("tern_app".into()), Some("tern".into()));
        assert_eq!(id.app_name, "Tern");
        assert!(!id.is_next);
        assert!(!id.is_bundled());
        assert_eq!(id.state_dir_name(), "tern");
        assert_eq!(id.tabbing_id(), "com.kaicui.tern.terminal");
    }

    #[test]
    fn stable_bundle_uses_stable_identity() {
        let id = Identity::resolve(
            Some("com.kaicui.tern".into()),
            Some("Tern".into()),
            Some("Tern".into()),
        );
        assert_eq!(id.app_name, "Tern");
        assert!(!id.is_next);
        assert!(id.is_bundled());
        assert_eq!(id.state_dir_name(), "tern");
        assert_eq!(id.tabbing_id(), "com.kaicui.tern.terminal");
    }

    #[test]
    fn next_bundle_uses_next_identity_and_isolated_state() {
        let id = Identity::resolve(
            Some(NEXT_BUNDLE_ID.into()),
            Some("Tern Next".into()),
            Some("Tern Next".into()),
        );
        assert_eq!(id.app_name, "Tern Next");
        assert!(id.is_next);
        assert!(id.is_bundled());
        assert_eq!(id.state_dir_name(), "tern-next");
        assert_eq!(id.tabbing_id(), "com.kaicui.tern.next.terminal");
    }

    #[test]
    fn missing_or_blank_bundle_names_use_variant_defaults() {
        let next = Identity::resolve(Some(NEXT_BUNDLE_ID.into()), Some("   ".into()), None);
        assert_eq!(next.app_name, "Tern Next");

        let from_bundle_name = Identity::resolve(
            Some(NEXT_BUNDLE_ID.into()),
            None,
            Some("Tern Preview".into()),
        );
        assert_eq!(from_bundle_name.app_name, "Tern Preview");
    }
}
