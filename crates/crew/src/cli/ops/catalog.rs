//! What crew is configured with and what it can run on: `crew config`,
//! `crew models` and `crew style`.

use anyhow::{anyhow, Result};
use crew_core::util::one_line;

use super::super::{ConfigCommand, ModelsCommand, StyleCommand};
use super::plain::members_line;
use super::Target;
use crate::endpoints::{Layers, Scope};
use crate::models::{self, Availability, Catalog};
use crate::styles::{self, Styles};

/// `crew config`: which files set the team's endpoints and keys, and what
/// they add up to.
pub fn config(target: &Target, command: ConfigCommand) -> Result<()> {
    let workspace = match &target.dir {
        Some(dir) => dir.clone(),
        None => std::env::current_dir()?,
    };
    let workspace = workspace.canonicalize().unwrap_or(workspace);
    match command {
        ConfigCommand::Show => {
            let layers = Layers::load(&workspace);
            print!("{}", layers.describe());
            // Exit non-zero for a rejected file, after showing which.
            layers.check()
        }
        ConfigCommand::Init { scope } => {
            let path = crate::endpoints::init(Scope::parse(&scope)?, &workspace)?;
            println!("wrote {}", path.display());
            println!("everything in it is commented out; edit it, then check with `crew config`");
            Ok(())
        }
    }
}

/// `crew models`: the model catalog and what can be launched from here.
pub fn models(command: ModelsCommand) -> Result<()> {
    let catalog = Catalog::load();
    let available = Availability::detect();
    match command {
        ModelsCommand::List => {
            let width = catalog
                .models
                .iter()
                .map(|(m, _)| m.key.len())
                .max()
                .unwrap_or(0);
            for (m, origin) in &catalog.models {
                let runtime = m.runtime.map(models::runtime_label).unwrap_or_default();
                let state = match available.check(m) {
                    Ok(()) => "available".to_string(),
                    Err(why) => format!("unavailable: {why}"),
                };
                println!(
                    "{:<width$}  {:<11} {:<8}  {:<22}  {state}{}",
                    m.key,
                    runtime,
                    m.tier,
                    m.price,
                    if *origin == "built-in" {
                        String::new()
                    } else {
                        format!("  [{origin}]")
                    },
                );
                println!("{:<width$}  {}", "", m.summary);
            }
            println!("\nwhich to pick, best first:");
            let work = catalog
                .picks
                .iter()
                .map(|p| p.work.len())
                .max()
                .unwrap_or(0);
            for pick in &catalog.picks {
                println!("  {:<work$}  {}", pick.work, pick.models.join(", "));
            }
            println!(
                "\nThe orchestrator gets this catalog in its briefing and picks a model per member. \
Profiles checked {}.",
                catalog.as_of
            );
            println!(
                "`crew models show <name>` has the detail and sources. Your own entries go in:"
            );
            for scope in [Scope::User, Scope::Global] {
                if let Some(path) = models::path(scope) {
                    println!("  {:<7} {}", scope.as_str(), path.display());
                }
            }
            for problem in &catalog.problems {
                eprintln!("warning: {problem}");
            }
            Ok(())
        }
        ModelsCommand::Show { name } => {
            let m = catalog.find(&name).ok_or_else(|| {
                let names: Vec<&str> = catalog.models.iter().map(|(m, _)| m.key.as_str()).collect();
                anyhow!(
                    "no model '{name}' in the catalog (there are: {})",
                    names.join(", ")
                )
            })?;
            println!("{}  ({})", m.key, m.name);
            println!(
                "runs on    {}",
                m.runtime.map(models::runtime_label).unwrap_or_default()
            );
            if !m.id.is_empty() {
                println!("model id   {}", m.id);
            }
            for (label, value) in [("tier", &m.tier), ("price", &m.price), ("speed", &m.speed)] {
                if !value.is_empty() {
                    println!("{label:<10} {value}");
                }
            }
            match available.check(m) {
                Ok(()) => println!("here       available"),
                Err(why) => println!("here       unavailable: {why}"),
            }
            println!("\n{}", m.summary);
            for (title, items) in [("good for", &m.good_for), ("not for", &m.avoid_for)] {
                if !items.is_empty() {
                    println!("\n{title}");
                    for item in items {
                        println!("  - {item}");
                    }
                }
            }
            let picked: Vec<&str> = catalog
                .picks
                .iter()
                .filter(|p| {
                    p.models
                        .iter()
                        .any(|n| catalog.find(n).is_some_and(|f| f.key == m.key))
                })
                .map(|p| p.work.as_str())
                .collect();
            if !picked.is_empty() {
                println!("\npicked for\n  {}", picked.join("; "));
            }
            if !m.notes.is_empty() {
                println!("\nnotes\n  {}", m.notes);
            }
            if !m.sources.is_empty() {
                println!("\nsources (checked {})", catalog.as_of);
                for source in &m.sources {
                    println!("  {source}");
                }
            }
            Ok(())
        }
        ModelsCommand::Init { scope } => {
            let path = models::init(Scope::parse(&scope)?)?;
            println!("wrote {}", path.display());
            println!("everything in it is commented out; edit it, then check with `crew models`");
            Ok(())
        }
    }
}

/// `crew style`: the work styles available in this workspace.
pub fn style(target: &Target, command: StyleCommand) -> Result<()> {
    let workspace = match &target.dir {
        Some(dir) => dir.clone(),
        None => std::env::current_dir()?,
    };
    let workspace = workspace.canonicalize().unwrap_or(workspace);
    match command {
        StyleCommand::List => {
            let all = Styles::load(&workspace);
            let name_width = all
                .entries
                .iter()
                .map(|e| e.style.name.len())
                .max()
                .unwrap_or(0);
            for entry in &all.entries {
                println!(
                    "{:<name_width$}  {:<8}  {}",
                    entry.style.name,
                    entry.origin.as_str(),
                    one_line(&entry.style.description)
                );
            }
            println!("\nstyle folders (a later one replaces a style of the same name):");
            for scope in Scope::ALL {
                if let Some(dir) = styles::dir(scope, &workspace) {
                    println!("  {:<8} {}", scope.as_str(), dir.display());
                }
            }
            println!("launch one with `crew --style <name>`, or pick it in the chooser (`crew`)");
            for problem in &all.problems {
                eprintln!("warning: {problem}");
            }
            Ok(())
        }
        StyleCommand::Show { name } => {
            let all = Styles::load(&workspace);
            let entry = all.require(&name)?;
            println!("{}  ({})", entry.style.name, entry.origin.as_str());
            if let Some(path) = &entry.path {
                println!("file       {}", path.display());
            }
            println!("team       {}", entry.staffing());
            println!("runtime    {}", entry.runtime());
            if let Some(saved) =
                styles::saved_team(&workspace, &name).filter(|s| !s.agents.is_empty())
            {
                println!("last time  {}", members_line(&saved.agents));
            }
            println!("\n{}", entry.source()?.trim_end());
            Ok(())
        }
        StyleCommand::New { name, scope, from } => {
            let path = styles::create(&workspace, &name, Scope::parse(&scope)?, from.as_deref())?;
            println!("wrote {}", path.display());
            println!(
                "edit it, then launch it with `crew --style {name}` or pick it in the chooser"
            );
            Ok(())
        }
    }
}
