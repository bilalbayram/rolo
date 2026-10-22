use std::collections::BTreeMap;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// User-defined fields stored on every record as a JSON object.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(transparent)]
pub struct CustomFields(BTreeMap<String, serde_json::Value>);

impl CustomFields {
    pub fn get(&self, key: &str) -> Option<&serde_json::Value> {
        self.0.get(key)
    }

    /// Apply a PATCH: keys set to null are removed, other keys are inserted or replaced.
    pub fn merge(&mut self, update: CustomFields) {
        for (key, value) in update.0 {
            if value.is_null() {
                self.0.remove(&key);
            } else {
                self.0.insert(key, value);
            }
        }
    }
}

impl IntoIterator for CustomFields {
    type Item = (String, serde_json::Value);
    type IntoIter = std::collections::btree_map::IntoIter<String, serde_json::Value>;

    fn into_iter(self) -> Self::IntoIter {
        self.0.into_iter()
    }
}

impl FromIterator<(String, serde_json::Value)> for CustomFields {
    fn from_iter<I: IntoIterator<Item = (String, serde_json::Value)>>(iter: I) -> Self {
        Self(iter.into_iter().collect())
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn merge_replaces_and_removes_keys() {
        let mut fields: CustomFields =
            serde_json::from_value(json!({"tier": "gold", "nps": 9})).expect("valid fields");
        let update: CustomFields =
            serde_json::from_value(json!({"nps": null, "owner": "ada"})).expect("valid fields");
        fields.merge(update);
        assert_eq!(
            serde_json::to_value(&fields).expect("serializable"),
            json!({"tier": "gold", "owner": "ada"})
        );
    }
}
