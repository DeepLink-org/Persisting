use crate::config::{ModelRoute, ProxyConfig};
use std::collections::HashMap;

#[derive(Debug, Clone)]
pub struct RouteTable {
    exact: HashMap<String, ModelRoute>,
    wildcard: Option<ModelRoute>,
    all: Vec<ModelRoute>,
}

impl RouteTable {
    pub fn from_config(config: &ProxyConfig) -> Self {
        let mut exact = HashMap::new();
        let mut wildcard = None;

        for route in &config.models {
            if route.name == "*" {
                wildcard = Some(route.clone());
            } else {
                exact.insert(route.name.clone(), route.clone());
            }
        }

        Self {
            exact,
            wildcard,
            all: config.models.clone(),
        }
    }

    pub fn resolve_model(&self, model: &str) -> Option<&ModelRoute> {
        self.exact.get(model).or(self.wildcard.as_ref())
    }

    pub fn list_models(&self) -> Vec<ModelInfo> {
        self.all
            .iter()
            .filter(|route| route.name != "*")
            .map(|route| ModelInfo {
                id: route.name.clone(),
                provider: route.provider.clone(),
                display_name: route
                    .display_name
                    .clone()
                    .unwrap_or_else(|| route.name.clone()),
            })
            .collect()
    }
}

#[derive(Debug, Clone)]
pub struct ModelInfo {
    pub id: String,
    pub provider: String,
    pub display_name: String,
}
