use crew_client::api::*;
use gtk::prelude::*;

use super::common;

#[derive(Clone)]
pub struct UsageView {
    pub root: gtk::ScrolledWindow,
    body: gtk::Box,
}
impl Default for UsageView {
    fn default() -> Self {
        Self::new()
    }
}

impl UsageView {
    pub fn new() -> Self {
        let body = common::column();
        Self {
            root: common::scroll(&body),
            body,
        }
    }
    pub fn update(&self, data: &UsagePresentation) {
        let offset = self.root.vadjustment().value();
        common::clear(&self.body);
        self.body.append(&common::heading("Usage"));
        if let Some(total) = &data.total {
            let tokens = u128::from(total.input_tokens)
                + u128::from(total.output_tokens)
                + u128::from(total.cache_read_tokens)
                + u128::from(total.cache_creation_tokens);
            common::fact(&self.body, "Total reported tokens", &tokens.to_string());
        } else {
            common::fact(
                &self.body,
                "Total reported tokens",
                if data.overflowed {
                    "Unavailable: reported total exceeds counter range"
                } else {
                    "Not reported"
                },
            );
        }
        self.body.append(&common::text(&format!(
            "{} of {} agents reporting",
            data.reporters, data.eligible
        )));
        for agent in &data.agents {
            self.body.append(&common::heading(&agent.agent_id));
            if let Some(usage) = &agent.usage {
                for (name, value) in [
                    ("Input", usage.input_tokens),
                    ("Output", usage.output_tokens),
                    ("Cache read", usage.cache_read_tokens),
                    ("Cache written", usage.cache_creation_tokens),
                    ("Turns", usage.turns),
                ] {
                    common::fact(&self.body, name, &value.to_string());
                }
            } else {
                self.body.append(&common::text("Token usage not reported"));
            }
            if let Some(context) = &agent.context {
                common::fact(
                    &self.body,
                    "Context",
                    &format!(
                        "{}% · {} of {} tokens",
                        context.percent, context.tokens, context.window
                    ),
                );
            } else {
                common::fact(&self.body, "Context", "Not reported");
            }
            common::fact(&self.body, "Compactions", &agent.compactions.to_string());
            for sample in &agent.samples {
                self.body.append(&common::text(&format!(
                    "Context sample {}: {}%",
                    sample.at_ms, sample.percent
                )));
            }
        }
        self.body.append(&common::text(
            "Context samples describe this client session only.",
        ));
        for group in &data.groups {
            self.body
                .append(&common::heading(&group.runtime.to_string()));
            for row in &group.quotas {
                common::fact(
                    &self.body,
                    &row.quota.window,
                    &format!(
                        "{}% used{}",
                        row.quota.percent_used,
                        if row.stale { " · stale report" } else { "" }
                    ),
                );
                common::fact(
                    &self.body,
                    "Reset",
                    &row.quota
                        .resets_at
                        .map_or("Not reported".into(), |at| at.to_string()),
                );
            }
            if !group.paused_agent_ids.is_empty() {
                self.body.append(&common::text(&format!(
                    "Paused at limit: {}",
                    group.paused_agent_ids.join(", ")
                )));
            }
        }
        self.root.vadjustment().set_value(offset);
    }
}
