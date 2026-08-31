use serde::{Deserialize, Serialize};

const ACCESS_KEY_STORAGE: &str = "pchronicle.catalog.access_key";
const SECRET_KEY_STORAGE: &str = "pchronicle.catalog.secret_key";

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct CatalogAuth {
    pub access_key: String,
    pub secret_key: String,
}

impl CatalogAuth {
    pub fn is_configured(&self) -> bool {
        !self.access_key.trim().is_empty() && !self.secret_key.trim().is_empty()
    }
}

pub fn load() -> CatalogAuth {
    CatalogAuth {
        access_key: read_storage(ACCESS_KEY_STORAGE).unwrap_or_default(),
        secret_key: read_storage(SECRET_KEY_STORAGE).unwrap_or_default(),
    }
}

pub fn save(auth: &CatalogAuth) {
    write_storage(ACCESS_KEY_STORAGE, auth.access_key.trim());
    write_storage(SECRET_KEY_STORAGE, auth.secret_key.trim());
}

pub fn credentials() -> Option<(String, String)> {
    let auth = load();
    auth.is_configured().then(|| {
        (
            auth.access_key.trim().to_owned(),
            auth.secret_key.trim().to_owned(),
        )
    })
}

fn read_storage(key: &str) -> Option<String> {
    let window = web_sys::window()?;
    let storage = window.local_storage().ok().flatten()?;
    storage.get_item(key).ok().flatten()
}

fn write_storage(key: &str, value: &str) {
    let Some(window) = web_sys::window() else {
        return;
    };
    let Some(storage) = window.local_storage().ok().flatten() else {
        return;
    };
    let _ = storage.set_item(key, value);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn configured_requires_both_keys() {
        assert!(!CatalogAuth::default().is_configured());
        assert!(
            !CatalogAuth {
                access_key: "ak".into(),
                secret_key: String::new(),
            }
            .is_configured()
        );
        assert!(
            CatalogAuth {
                access_key: "ak".into(),
                secret_key: "sk".into(),
            }
            .is_configured()
        );
    }
}
