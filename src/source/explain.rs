use std::collections::{BTreeMap, BTreeSet};

use crate::{LockFile, ResolvedPackage, closure_by_provider};

/// Print the dependency closure as a tree rooted at the direct packages.
pub(crate) fn print_tree(lock: &LockFile) {
    let by_provider = closure_by_provider(lock);
    let mut roots: Vec<&ResolvedPackage> =
        lock.closure.iter().filter(|entry| entry.direct).collect();
    roots.sort_by(|a, b| a.provider.cmp(&b.provider));

    println!("{} ({} providers)", lock.root, lock.closure.len());
    let mut shown = BTreeSet::new();
    for (index, root) in roots.iter().enumerate() {
        tree_node(root, &by_provider, "", index + 1 == roots.len(), &mut shown);
    }
}

fn tree_node(
    node: &ResolvedPackage,
    by_provider: &BTreeMap<&str, &ResolvedPackage>,
    prefix: &str,
    last: bool,
    shown: &mut BTreeSet<String>,
) {
    let connector = if last { "└─ " } else { "├─ " };
    let satisfies = if node.satisfies.is_empty() {
        String::new()
    } else {
        format!("  ({})", node.satisfies.join(", "))
    };
    if !shown.insert(node.provider.clone()) {
        println!("{prefix}{connector}{} (*)", node.provider);
        return;
    }
    println!("{prefix}{connector}{}{satisfies}", node.provider);

    let child_prefix = format!("{prefix}{}", if last { "   " } else { "│  " });
    let mut deps: Vec<&ResolvedPackage> = node
        .dependencies
        .iter()
        .filter_map(|dep| by_provider.get(dep.as_str()).copied())
        .collect();
    deps.sort_by(|a, b| a.provider.cmp(&b.provider));
    for (index, dep) in deps.iter().enumerate() {
        tree_node(
            dep,
            by_provider,
            &child_prefix,
            index + 1 == deps.len(),
            shown,
        );
    }
}

/// Explain why a provider is in the closure by walking `requested_by` upward.
pub(crate) fn print_why(lock: &LockFile, target: &str) {
    let by_provider = closure_by_provider(lock);
    let Some(entry) = by_provider.get(target) else {
        println!("{target} is not in the closure");
        return;
    };
    println!("{target}  ({})", entry.version);
    let mut seen = BTreeSet::new();
    why_chain(entry, &by_provider, 1, &mut seen);
}

fn why_chain(
    node: &ResolvedPackage,
    by_provider: &BTreeMap<&str, &ResolvedPackage>,
    depth: usize,
    seen: &mut BTreeSet<String>,
) {
    let indent = "  ".repeat(depth);
    if node.direct {
        let satisfies = if node.satisfies.is_empty() {
            String::new()
        } else {
            format!(" \\usepackage{{{}}}", node.satisfies.join(", "))
        };
        println!("{indent}↑ requested directly{satisfies}");
    }
    for parent in &node.requested_by {
        println!("{indent}↑ {parent}");
        if seen.insert(format!("{}<-{parent}", node.provider))
            && let Some(parent_entry) = by_provider.get(parent.as_str())
        {
            why_chain(parent_entry, by_provider, depth + 1, seen);
        }
    }
}

pub(crate) fn print_explanation(lock: &LockFile) {
    println!("root: {}", lock.root);
    if let Some(document_class) = &lock.document_class {
        print!("document class: {}", document_class.name);
        if !document_class.options.is_empty() {
            print!(" [{}]", document_class.options.join(", "));
        }
        println!(
            " ({}:{})",
            document_class.source.path, document_class.source.line
        );
    } else {
        println!("document class: not found");
    }
    println!("sources: {}", lock.sources.len());
    println!("packages: {}", lock.packages.len());
    for package in &lock.packages {
        let options = if package.options.is_empty() {
            String::new()
        } else {
            format!(" [{}]", package.options.join(", "))
        };
        println!(
            "  - {}{} via \\{} at {}:{}",
            package.name, options, package.command, package.source.path, package.source.line
        );
    }
    println!("inputs: {}", lock.inputs.len());
    for input in &lock.inputs {
        println!(
            "  - {:?} {} -> {}",
            input.kind,
            input.name,
            input.resolved_path.as_deref().unwrap_or("<unresolved>")
        );
    }
    println!("bibliography declarations: {}", lock.bibliographies.len());
    println!("graphics declarations: {}", lock.graphics.len());
    if !lock.unresolved.is_empty() {
        println!("unresolved: {}", lock.unresolved.len());
        for item in &lock.unresolved {
            println!(
                "  - {} `{}` at {}:{}",
                item.kind, item.name, item.source.path, item.source.line
            );
        }
    }
}
