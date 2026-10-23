use std::collections::{BTreeMap, BTreeSet};

use clap::{Arg, ArgMatches, Command};
use clapham::{ComposedSet, Refusal};

use crate::{configuration, input::Field};

pub(crate) const AS: &str = "cli:as";
pub(crate) const JSON: &str = "cli:json";

#[derive(Debug, Clone, Default)]
pub(crate) struct Node {
    pub(crate) operation: Option<String>,
    pub(crate) fields: Vec<Field>,
    children: BTreeMap<String, Node>,
}

impl Node {
    pub(crate) fn from_set(set: &ComposedSet) -> Result<Self, Refusal> {
        let mut root = Self::default();
        for operation in set.operations() {
            let mut node = &mut root;
            for segment in operation.name.as_str().split('.') {
                if !valid_name(segment) {
                    return Err(configuration(format!(
                        "operation {} has an unrepresentable command segment",
                        operation.name,
                    )));
                }
                node = node.children.entry(segment.into()).or_default();
            }
            node.operation = Some(operation.name.to_string());
            node.fields = crate::input::fields(operation)?;
        }
        // Keep the complete built-in path as well as its top-level spelling.
        for name in ["describe", "completions"] {
            if root.children.contains_key(name) {
                return Err(configuration(format!(
                    "{name} collides with a built-in command"
                )));
            }
        }
        let describe = root.children["clapham"].children["describe"].clone();
        root.children.insert("describe".into(), describe);
        Ok(root)
    }

    pub(crate) fn command(&self, name: &str, set: &ComposedSet) -> Result<Command, Refusal> {
        if name.trim().is_empty() {
            return Err(configuration("the command name is empty"));
        }
        let mut command = Command::new(name.to_owned())
            .disable_help_subcommand(true)
            .subcommand_precedence_over_arg(true);
        if let Some(operation) = &self.operation {
            let descriptor = set
                .get(operation)
                .expect("the tree uses composed operations")
                .descriptor();
            let text = descriptor.title.as_ref().map_or_else(
                || descriptor.description.clone(),
                |title| format!("{title}\n\n{}", descriptor.description),
            );
            let effect = serde_json::to_value(descriptor.effect).expect("effects serialise");
            command = command
                .about(text)
                .after_help(format!(
                    "Effect: {}",
                    effect.as_str().expect("effect is a string")
                ))
                .subcommand_negates_reqs(true)
                .args_conflicts_with_subcommands(true)
                .arg(
                    Arg::new(JSON)
                        .long("json")
                        .value_name("DOCUMENT|-")
                        .help("Supply the whole input object as JSON, or - to read stdin")
                        .conflicts_with_all(self.fields.iter().map(|field| field.id.clone())),
                );
            for field in &self.fields {
                let mut arg = Arg::new(field.id.clone())
                    .value_name(field.kind.value_name())
                    .help(field.help.clone());
                arg = if let Some(index) = field.position {
                    arg.index(index).value_name(field.property.clone())
                } else {
                    arg.long(field.flag.clone())
                };
                if field.required {
                    arg = arg.required_unless_present(JSON);
                }
                if field.kind.numeric() {
                    arg = arg.allow_negative_numbers(true);
                }
                command = command.arg(arg);
            }
        } else {
            command = command.subcommand_required(true);
        }
        for (segment, child) in &self.children {
            command = command.subcommand(child.command(segment, set)?);
        }
        Ok(command)
    }

    pub(crate) fn selected<'a>(&'a self, matches: &'a ArgMatches) -> (&'a Node, &'a ArgMatches) {
        let mut node = self;
        let mut matches = matches;
        while let Some((name, child)) = matches.subcommand() {
            node = &node.children[name];
            matches = child;
        }
        (node, matches)
    }
}

pub(crate) fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && !name.starts_with('-')
        && !name
            .chars()
            .any(|c| c.is_whitespace() || c.is_control() || c == '=')
}

pub(crate) fn check_flags(fields: &[Field]) -> Result<(), Refusal> {
    let mut flags = BTreeSet::from([
        "help", "json", "as", "yes", "dry-run", "timeout", "format", "raw",
    ]);
    for field in fields.iter().filter(|field| field.position.is_none()) {
        if !valid_name(&field.flag) || !flags.insert(&field.flag) {
            return Err(configuration(format!(
                "unrepresentable or colliding flag --{}; use a different property name",
                field.flag
            )));
        }
    }
    Ok(())
}
