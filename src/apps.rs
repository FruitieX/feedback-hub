//! The app registry tells agents which repository and checkout an app id maps to.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct AppInfo {
    pub id: String,
    #[serde(default)]
    pub name: Option<String>,
    /// Git remote, e.g. `git@github.com:FruitieX/homectl.git`.
    #[serde(default)]
    pub repo: Option<String>,
    /// Path of the checkout on the agent machine; `~` is expanded by the worker.
    #[serde(default)]
    pub checkout: Option<String>,
    /// Hosts whose pages get the widget injected (informational for agents).
    #[serde(default)]
    pub hosts: Vec<String>,
    /// Free-form hints for agents, e.g. where the UI code lives.
    #[serde(default)]
    pub notes: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
struct Registry {
    #[serde(default)]
    apps: Vec<AppInfo>,
}

pub fn parse(text: &str) -> anyhow::Result<Vec<AppInfo>> {
    Ok(toml::from_str::<Registry>(text)?.apps)
}

#[cfg(test)]
mod tests {
    #[test]
    fn parses_registry() {
        let apps = super::parse(
            r#"
            [[apps]]
            id = "homectl"
            repo = "git@github.com:FruitieX/homectl.git"
            hosts = ["homectl.fruitiex.org"]
            "#,
        )
        .unwrap();
        assert_eq!(apps[0].id, "homectl");
        assert_eq!(apps[0].hosts, vec!["homectl.fruitiex.org"]);
        assert_eq!(apps[0].checkout, None);
    }
}
