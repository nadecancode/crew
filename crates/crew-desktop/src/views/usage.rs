use crew_client::api::*;
use gtk::prelude::*;

use super::common::{self, Facts};

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
        body.set_spacing(24);
        Self {
            root: common::scroll(&body),
            body,
        }
    }
    pub fn update(&self, data: &UsagePresentation) {
        let offset = self.root.vadjustment().value();
        common::clear(&self.body);
        let summary = gtk::Box::new(gtk::Orientation::Vertical, 12);
        summary.append(&common::title("Usage"));
        let total = match &data.total {
            Some(total) => Some(count(
                u128::from(total.input_tokens)
                    + u128::from(total.output_tokens)
                    + u128::from(total.cache_read_tokens)
                    + u128::from(total.cache_creation_tokens),
            )),
            None if data.overflowed => {
                Some("Unavailable: the reported total exceeds the counter range".into())
            }
            None => None,
        };
        summary.append(
            &Facts::with(&[
                ("Total reported tokens", total),
                (
                    "Reporting",
                    Some(format!(
                        "{} of {} agents report usage",
                        data.reporters, data.eligible
                    )),
                ),
            ])
            .root,
        );
        self.body.append(&summary);
        // Unknown is not zero: an agent that reports nothing gets no rows of
        // "not reported" figures, only its name in one line.
        let (reporting, silent): (Vec<_>, Vec<_>) = data.agents.iter().partition(|agent| {
            agent.usage.is_some()
                || agent.context.is_some()
                || agent.compactions > 0
                || !agent.samples.is_empty()
        });
        for agent in &reporting {
            let section = gtk::Box::new(gtk::Orientation::Vertical, 12);
            section.append(&common::heading(&agent.agent_id));
            let usage = agent.usage.as_ref();
            section.append(
                &Facts::with(&[
                    ("Input", usage.map(|u| count(u.input_tokens.into()))),
                    ("Output", usage.map(|u| count(u.output_tokens.into()))),
                    ("Cache read", usage.map(|u| count(u.cache_read_tokens.into()))),
                    (
                        "Cache written",
                        usage.map(|u| count(u.cache_creation_tokens.into())),
                    ),
                    ("Turns", usage.map(|u| count(u.turns.into()))),
                    (
                        "Context",
                        agent.context.as_ref().map(|context| {
                            format!(
                                "{}% · {} of {} tokens",
                                context.percent,
                                count(context.tokens.into()),
                                count(context.window.into())
                            )
                        }),
                    ),
                    (
                        "Compactions",
                        (agent.compactions > 0).then(|| agent.compactions.to_string()),
                    ),
                    (
                        "Context this session",
                        (!agent.samples.is_empty()).then(|| {
                            agent
                                .samples
                                .iter()
                                .map(|sample| {
                                    format!("{}% at {}", sample.percent, clock(sample.at_ms))
                                })
                                .collect::<Vec<_>>()
                                .join(", ")
                        }),
                    ),
                ])
                .root,
            );
            self.body.append(&section);
        }
        if !silent.is_empty() {
            self.body.append(&common::muted(&format!(
                "Not reported by {}",
                silent
                    .iter()
                    .map(|agent| agent.agent_id.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            )));
        }
        if reporting.iter().any(|agent| !agent.samples.is_empty()) {
            self.body.append(&common::muted(
                "Context samples describe this client session only.",
            ));
        }
        for group in &data.groups {
            let section = gtk::Box::new(gtk::Orientation::Vertical, 12);
            section.append(&common::heading(&format!(
                "{} limits",
                group.runtime
            )));
            let facts: Vec<(&str, Option<String>)> = group
                .quotas
                .iter()
                .map(|row| {
                    (
                        row.quota.window.as_str(),
                        Some(format!(
                            "{:.1}% used{}{}",
                            row.quota.percent_used,
                            row.quota.resets_at.map_or(String::new(), |at| format!(
                                " · resets {}",
                                common::timestamp(at)
                            )),
                            if row.stale { " · stale report" } else { "" }
                        )),
                    )
                })
                .collect();
            section.append(&Facts::with(&facts).root);
            if !group.paused_agent_ids.is_empty() {
                section.append(&common::text(&format!(
                    "Paused at the limit: {}",
                    group.paused_agent_ids.join(", ")
                )));
            }
            self.body.append(&section);
        }
        self.root.vadjustment().set_value(offset);
    }
}

/// A token count with thousands separators.
fn count(value: u128) -> String {
    let digits = value.to_string();
    let mut grouped = String::with_capacity(digits.len() + digits.len() / 3);
    for (index, digit) in digits.chars().enumerate() {
        if index > 0 && (digits.len() - index).is_multiple_of(3) {
            grouped.push(',');
        }
        grouped.push(digit);
    }
    grouped
}

fn clock(at_ms: i64) -> String {
    gtk::glib::DateTime::from_unix_local(at_ms.div_euclid(1000))
        .and_then(|date| date.format("%H:%M"))
        .map_or_else(|_| common::timestamp(at_ms), |value| value.to_string())
}

#[cfg(test)]
mod tests {
    #[test]
    fn counts_group_thousands() {
        assert_eq!(super::count(0), "0");
        assert_eq!(super::count(999), "999");
        assert_eq!(super::count(1_000), "1,000");
        assert_eq!(super::count(12_345_678), "12,345,678");
    }
}
