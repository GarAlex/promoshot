//! Credits from resource `provenance` (docs/BEAT-SYNC.md §8): which tools
//! and licences made the media a project — or one render of it — uses,
//! with costs summed by currency, so an agent can write the AI label and
//! the credits from what was actually used.

use std::collections::{BTreeMap, HashSet};

use promo_model::ProjectMetadata;
use serde_json::{json, Value};

/// One tool and licence, and what it made.
#[derive(Debug, Clone, PartialEq)]
pub struct Credit {
    pub tool: String,
    pub license: String,
    /// The resources, by display name.
    pub resources: Vec<String>,
    /// Costs, summed per currency.
    pub cost: BTreeMap<String, f64>,
}

/// The resources a render of `span` (seconds, inclusive) reaches — or, with
/// no span, the project uses at all: what the layers in it point at, then
/// what those resources point at in turn (a composition's layers, a model's
/// slots), and what the settings name.
pub fn used_resources(meta: &ProjectMetadata, span: Option<(f64, f64)>) -> HashSet<String> {
    let mut used = HashSet::new();
    let Ok(document) = serde_json::to_value(meta) else {
        return used;
    };
    match span {
        None => crate::inspect::pointed_at(&document, None, &mut used),
        Some((from, to)) => {
            crate::inspect::pointed_at(&document["compositionSettings"], None, &mut used);
            for layer in meta.layers.iter().flatten() {
                let start = layer.start_time;
                let end = layer.duration.map_or(f64::INFINITY, |d| start + d);
                if layer.is_enabled && start <= to && end >= from {
                    if let Ok(value) = serde_json::to_value(layer) {
                        crate::inspect::pointed_at(&value, None, &mut used);
                    }
                }
            }
            loop {
                let before = used.len();
                for resource in meta.resources.iter().flatten() {
                    if used.contains(&resource.id) {
                        if let Ok(value) = serde_json::to_value(resource) {
                            let mut more = HashSet::new();
                            crate::inspect::pointed_at(&value, None, &mut more);
                            used.extend(more);
                        }
                    }
                }
                if used.len() == before {
                    break;
                }
            }
        }
    }
    used
}

/// The credits of the resources `span` reaches (see [`used_resources`]),
/// grouped by tool and licence.
pub fn credits(meta: &ProjectMetadata, span: Option<(f64, f64)>) -> Vec<Credit> {
    let used = used_resources(meta, span);
    let mut groups: BTreeMap<(String, String), Credit> = BTreeMap::new();
    for resource in meta.resources.iter().flatten() {
        let Some(made) = resource.provenance.as_ref() else {
            continue;
        };
        if !used.contains(&resource.id) {
            continue;
        }
        let tool = made
            .tool
            .clone()
            .unwrap_or_else(|| "an unnamed tool".into());
        let license = made
            .license
            .clone()
            .unwrap_or_else(|| "no licence stated".into());
        let entry = groups
            .entry((tool.clone(), license.clone()))
            .or_insert_with(|| Credit {
                tool,
                license,
                resources: Vec::new(),
                cost: BTreeMap::new(),
            });
        entry.resources.push(if resource.display_name.is_empty() {
            resource.id.clone()
        } else {
            resource.display_name.clone()
        });
        if let Some(cost) = made.cost {
            let currency = made.currency.clone().unwrap_or_else(|| "USD".into());
            *entry.cost.entry(currency).or_default() += cost;
        }
    }
    groups.into_values().collect()
}

/// The credits as JSON: `[{"tool", "license", "resources", "cost"}]`.
pub fn json(credits: &[Credit]) -> Value {
    Value::Array(
        credits
            .iter()
            .map(|c| {
                json!({ "tool": c.tool, "license": c.license, "resources": c.resources,
                        "cost": c.cost })
            })
            .collect(),
    )
}

/// The credits as lines: `ace-step (Apache-2.0): Song — 0.00 USD`.
pub fn text(credits: &[Credit]) -> String {
    credits
        .iter()
        .map(|c| {
            let cost: Vec<String> = c
                .cost
                .iter()
                .map(|(currency, sum)| format!("{sum:.2} {currency}"))
                .collect();
            format!(
                "  {} ({}): {}{}",
                c.tool,
                c.license,
                c.resources.join(", "),
                if cost.is_empty() {
                    String::new()
                } else {
                    format!(" — {}", cost.join(", "))
                }
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}
