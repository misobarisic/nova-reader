//! Bounded source-defined browse filters. Display labels remain separate from
//! values sent back to the provider, including options whose value is empty.
use serde_json::{Map, Value};
use slint::{ModelRc, SharedString, VecModel};
use std::collections::BTreeSet;

const MAX_FILTERS: usize = 32;
const MAX_OPTIONS: usize = 200;
const MAX_ID: usize = 128;
const MAX_LABEL: usize = 256;
const MAX_VALUE: usize = 2048;

#[derive(Default)]
pub struct FilterSet {
    notice: String,
    filters: Vec<Filter>,
}

struct Filter {
    id: String,
    name: String,
    kind: Kind,
}

enum Kind {
    Select {
        options: Vec<OptionValue>,
        selected: usize,
    },
    Text(String),
    Bool(bool),
}

struct OptionValue {
    label: String,
    value: String,
}

impl FilterSet {
    pub fn parse(value: Value) -> Result<Self, String> {
        let object = value
            .as_object()
            .ok_or("Invalid source filter definitions")?;
        let notice = match object.get("notice") {
            Some(value) => bounded_string(value, MAX_VALUE, true)
                .ok_or("Invalid source filter notice")?
                .to_owned(),
            None => String::new(),
        };
        let definitions = object
            .get("filters")
            .and_then(Value::as_array)
            .ok_or("Missing source filter definitions")?;
        if definitions.len() > MAX_FILTERS {
            return Err("Source has too many filters".into());
        }
        let mut filters = Vec::with_capacity(definitions.len());
        let mut ids = BTreeSet::new();
        let mut names = BTreeSet::new();
        for definition in definitions {
            let definition = definition
                .as_object()
                .ok_or("Invalid source filter definition")?;
            let id = definition
                .get("id")
                .and_then(|value| bounded_string(value, MAX_ID, false))
                .filter(|id| !id.chars().any(char::is_whitespace))
                .ok_or("Invalid source filter ID")?
                .to_owned();
            if !ids.insert(id.clone()) {
                return Err("Duplicate source filter ID".into());
            }
            let name = match definition.get("name").or_else(|| definition.get("label")) {
                Some(value) => bounded_string(value, MAX_LABEL, false)
                    .ok_or("Invalid source filter name")?
                    .to_owned(),
                None => readable_id(&id),
            };
            if !valid_string(&name, MAX_LABEL, false) {
                return Err("Invalid source filter name".into());
            }
            if !names.insert(name.clone()) {
                return Err("Duplicate source filter name".into());
            }
            let default = definition.get("default");
            let kind = match definition.get("type").and_then(Value::as_str) {
                Some("select") => {
                    let values = definition
                        .get("options")
                        .and_then(Value::as_array)
                        .filter(|values| !values.is_empty() && values.len() <= MAX_OPTIONS)
                        .ok_or("Invalid source filter options")?;
                    let mut options = Vec::with_capacity(values.len());
                    let mut unique_values = BTreeSet::new();
                    for option in values {
                        let label = option
                            .get("label")
                            .and_then(|value| bounded_string(value, MAX_LABEL, false))
                            .ok_or("Invalid source filter option label")?
                            .to_owned();
                        let value = option
                            .get("value")
                            .and_then(|value| bounded_string(value, MAX_VALUE, true))
                            .ok_or("Invalid source filter option value")?
                            .to_owned();
                        if !unique_values.insert(value.clone()) {
                            return Err("Duplicate source filter option value".into());
                        }
                        options.push(OptionValue { label, value });
                    }
                    let selected = default
                        .and_then(Value::as_str)
                        .and_then(|value| options.iter().position(|option| option.value == value))
                        .unwrap_or(0);
                    Kind::Select { options, selected }
                }
                Some("text") => Kind::Text(
                    default
                        .and_then(|value| bounded_string(value, MAX_VALUE, true))
                        .unwrap_or_default()
                        .to_owned(),
                ),
                Some("bool") => Kind::Bool(default.and_then(Value::as_bool).unwrap_or(false)),
                _ => return Err("Unsupported source filter type".into()),
            };
            filters.push(Filter { id, name, kind });
        }
        Ok(Self { notice, filters })
    }

    pub fn notice(&self) -> &str {
        &self.notice
    }

    pub fn rows(&self) -> Vec<crate::SettingRow> {
        self.filters
            .iter()
            .map(|filter| {
                let mut row = crate::SettingRow {
                    key: filter.id.as_str().into(),
                    name: filter.name.as_str().into(),
                    ..Default::default()
                };
                match &filter.kind {
                    Kind::Select { options, selected } => {
                        row.kind = "select".into();
                        row.value = options[*selected].value.as_str().into();
                        row.selected = *selected as i32;
                        row.options = ModelRc::new(VecModel::from(
                            options
                                .iter()
                                .map(|option| SharedString::from(option.label.as_str()))
                                .collect::<Vec<_>>(),
                        ));
                    }
                    Kind::Text(value) => {
                        row.kind = "text".into();
                        row.value = value.as_str().into();
                    }
                    Kind::Bool(checked) => {
                        row.kind = "bool".into();
                        row.checked = *checked;
                        row.value = if *checked { "true" } else { "false" }.into();
                    }
                }
                row
            })
            .collect()
    }

    pub fn change(&mut self, key: &str, value: &str) -> bool {
        let Some(filter) = self.filters.iter_mut().find(|filter| filter.id == key) else {
            return false;
        };
        match &mut filter.kind {
            Kind::Select { options, selected } => {
                let Some(index) = options.iter().position(|option| option.value == value) else {
                    return false;
                };
                replace_if_changed(selected, index)
            }
            Kind::Text(current) if valid_string(value, MAX_VALUE, true) => {
                replace_if_changed(current, value.to_owned())
            }
            Kind::Bool(current) => match value {
                "true" => replace_if_changed(current, true),
                "false" => replace_if_changed(current, false),
                _ => false,
            },
            _ => false,
        }
    }

    pub fn select(&mut self, key: &str, index: i32) -> bool {
        let Some(Filter {
            kind: Kind::Select { options, selected },
            ..
        }) = self.filters.iter_mut().find(|filter| filter.id == key)
        else {
            return false;
        };
        let Ok(index) = usize::try_from(index) else {
            return false;
        };
        index < options.len() && replace_if_changed(selected, index)
    }

    pub fn values(&self) -> Value {
        Value::Object(
            self.filters
                .iter()
                .map(|filter| {
                    let value = match &filter.kind {
                        Kind::Select { options, selected } => {
                            Value::String(options[*selected].value.clone())
                        }
                        Kind::Text(value) => Value::String(value.clone()),
                        Kind::Bool(value) => Value::Bool(*value),
                    };
                    (filter.id.clone(), value)
                })
                .collect::<Map<_, _>>(),
        )
    }
}

fn bounded_string(value: &Value, max: usize, empty: bool) -> Option<&str> {
    value
        .as_str()
        .filter(|value| valid_string(value, max, empty))
}

fn valid_string(value: &str, max: usize, empty: bool) -> bool {
    value.len() <= max
        && (empty || !value.trim().is_empty())
        && !value.chars().any(char::is_control)
}

fn replace_if_changed<T: PartialEq>(current: &mut T, next: T) -> bool {
    if *current == next {
        return false;
    }
    *current = next;
    true
}

fn readable_id(id: &str) -> String {
    id.split(['_', '-'])
        .map(|word| {
            let mut characters = word.chars();
            characters
                .next()
                .map(|first| first.to_uppercase().collect::<String>() + characters.as_str())
                .unwrap_or_default()
        })
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use slint::Model;

    #[test]
    fn selection_sends_provider_values_and_preserves_empty_all_option() {
        let mut filters =
            FilterSet::parse(json!({"notice":"Ignored with text search", "filters":[{
                "id":"genre", "type":"select", "options":[
                    {"label":"All", "value":""}, {"label":"Adventure", "value":"genre/adventure"}
                ]
            }]}))
            .unwrap();
        assert_eq!(filters.notice(), "Ignored with text search");
        assert_eq!(filters.values(), json!({"genre":""}));
        assert_eq!(filters.rows()[0].options.row_data(1).unwrap(), "Adventure");
        assert!(filters.select("genre", 1));
        assert_eq!(filters.values(), json!({"genre":"genre/adventure"}));
        assert!(!filters.change("genre", "All"));
        assert!(!filters.select("genre", -1));
        assert!(!filters.select("genre", 2));
        assert!(filters.change("genre", ""));
        assert_eq!(filters.values(), json!({"genre":""}));
    }

    #[test]
    fn defaults_and_edits_keep_json_types() {
        let mut filters = FilterSet::parse(json!({"filters":[
            {"id":"sort", "type":"select", "default":"new", "options":[{"label":"Popular","value":"popular"},{"label":"Newest","value":"new"}]},
            {"id":"author", "type":"text", "default":"Ada"},
            {"id":"completed", "type":"bool", "default":true}
        ]})).unwrap();
        assert_eq!(filters.rows()[0].selected, 1);
        assert_eq!(
            filters.values(),
            json!({"sort":"new","author":"Ada","completed":true})
        );
        assert!(filters.change("author", "Grace"));
        assert!(filters.change("completed", "false"));
        assert!(!filters.change("completed", "0"));
        assert!(!filters.change("author", "Grace"));
        assert!(!filters.change("unknown", "value"));
        assert_eq!(
            filters.values(),
            json!({"sort":"new","author":"Grace","completed":false})
        );
    }

    #[test]
    fn invalid_defaults_fall_back_to_first_empty_and_false() {
        let filters = FilterSet::parse(json!({"filters":[
            {"id":"sort", "type":"select", "default":"missing", "options":[{"label":"All","value":""}]},
            {"id":"query", "type":"text", "default":123},
            {"id":"complete", "type":"bool", "default":"true"}
        ]})).unwrap();
        assert_eq!(
            filters.values(),
            json!({"sort":"","query":"","complete":false})
        );
        assert_eq!(FilterSet::default().values(), json!({}));
    }

    #[test]
    fn rejects_ambiguous_and_unsupported_definitions() {
        for filters in [
            json!([{"id":"genre","type":"text"},{"id":"genre","type":"bool"}]),
            json!([{"id":"first","name":"Same","type":"text"},{"id":"second","name":"Same","type":"bool"}]),
            json!([{"id":"sort","type":"select","options":[{"label":"A","value":"a"},{"label":"B","value":"a"}]}]),
            json!([{"id":"sort","type":"select","options":[]}]),
            json!([{"id":"kind","type":"javascript"}]),
            json!([{"id":"","type":"text"}]),
            json!([{"id":"query","name":false,"type":"text"}]),
        ] {
            assert!(FilterSet::parse(json!({"filters":filters})).is_err());
        }
        assert!(FilterSet::parse(json!([])).is_err());
    }

    #[test]
    fn enforces_filter_option_and_string_limits() {
        let filters = (0..33)
            .map(|i| json!({"id":format!("filter{i}"),"type":"text"}))
            .collect::<Vec<_>>();
        assert!(FilterSet::parse(json!({"filters":filters})).is_err());
        let options = (0..201)
            .map(|i| json!({"label":i.to_string(),"value":i.to_string()}))
            .collect::<Vec<_>>();
        assert!(
            FilterSet::parse(json!({"filters":[{"id":"genre","type":"select","options":options}]}))
                .is_err()
        );
        assert!(
            FilterSet::parse(json!({"filters":[{"id":"x".repeat(MAX_ID+1),"type":"text"}]}))
                .is_err()
        );
        let mut filters =
            FilterSet::parse(json!({"filters":[{"id":"query","type":"text"}]})).unwrap();
        assert!(!filters.change("query", &"x".repeat(MAX_VALUE + 1)));
        assert!(!filters.change("query", "hidden\u{0}text"));
        assert_eq!(filters.values(), json!({"query":""}));
    }
}
