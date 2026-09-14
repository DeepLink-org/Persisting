use serde::{Deserialize, Serialize};
const ACCESS_KEY_STORAGE: &str = "pchronicle.catalog.access_key";
const SECRET_KEY_STORAGE: &str = "pchronicle.catalog.secret_key";
const ACCOUNTS_STORAGE: &str = "pchronicle.catalog.accounts";
const ACTIVE_STORAGE: &str = "pchronicle.catalog.active";

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct CatalogAuth {
    #[serde(default)]
    pub label: String,
    pub access_key: String,
    pub secret_key: String,
}
impl CatalogAuth {
    pub fn is_configured(&self) -> bool {
        !self.access_key.trim().is_empty() && !self.secret_key.trim().is_empty()
    }
}
pub fn accounts() -> Vec<CatalogAuth> {
    let values = read_storage(ACCOUNTS_STORAGE)
        .and_then(|v| serde_json::from_str(&v).ok())
        .unwrap_or_else(|| {
            let legacy = CatalogAuth {
                label: String::new(),
                access_key: read_storage(ACCESS_KEY_STORAGE).unwrap_or_default(),
                secret_key: read_storage(SECRET_KEY_STORAGE).unwrap_or_default(),
            };
            legacy
                .is_configured()
                .then_some(vec![legacy])
                .unwrap_or_default()
        });
    values
        .into_iter()
        .map(|mut auth: CatalogAuth| {
            if auth.label.trim().is_empty() {
                auth.label = auth.access_key.clone();
            }
            auth
        })
        .collect()
}
pub fn load() -> CatalogAuth {
    let values = accounts();
    read_storage(ACTIVE_STORAGE)
        .and_then(|v| v.parse::<usize>().ok())
        .and_then(|i| values.get(i).cloned())
        .unwrap_or_else(|| values.into_iter().next().unwrap_or_default())
}
pub fn select(index: usize) {
    write_storage(ACTIVE_STORAGE, &index.to_string());
}
pub fn save_at(selected: Option<usize>, auth: &CatalogAuth) -> usize {
    let auth = CatalogAuth {
        label: auth.label.trim().to_owned(),
        access_key: auth.access_key.trim().to_owned(),
        secret_key: auth.secret_key.trim().to_owned(),
    };
    let mut values = accounts();
    let index = selected.filter(|i| *i < values.len()).unwrap_or_else(|| {
        values
            .iter()
            .position(|item| item.access_key == auth.access_key)
            .unwrap_or(values.len())
    });
    if index == values.len() {
        values.push(auth.clone());
    } else {
        values[index] = auth.clone();
    }
    select(index);
    if let Ok(json) = serde_json::to_string(&values) {
        write_storage(ACCOUNTS_STORAGE, &json);
    }
    write_storage(ACCESS_KEY_STORAGE, &auth.access_key);
    write_storage(SECRET_KEY_STORAGE, &auth.secret_key);
    index
}
pub fn remove(index: usize) {
    let mut values = accounts();
    if index >= values.len() {
        return;
    }
    values.remove(index);
    if let Ok(json) = serde_json::to_string(&values) {
        write_storage(ACCOUNTS_STORAGE, &json);
    }
    if values.is_empty() {
        write_storage(ACTIVE_STORAGE, "0");
        write_storage(ACCESS_KEY_STORAGE, "");
        write_storage(SECRET_KEY_STORAGE, "");
    } else {
        select(index.min(values.len() - 1));
    }
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
    if let Some(window) = web_sys::window()
        && let Some(storage) = window.local_storage().ok().flatten()
    {
        let _ = storage.set_item(key, value);
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn configured_requires_both_keys() {
        assert!(!CatalogAuth::default().is_configured());
        assert!(
            !CatalogAuth {
                label: String::new(),
                access_key: "ak".into(),
                secret_key: String::new()
            }
            .is_configured()
        );
        assert!(
            CatalogAuth {
                label: String::new(),
                access_key: "ak".into(),
                secret_key: "sk".into()
            }
            .is_configured()
        );
    }
}
