//! `types` writes the TypeScript declarations of the services marked
//! `#[rutis::service]` that `dim-app-server` serves (rutis usage §3.4) to
//! `npm/branch-services.d.ts`, and the addresses a plugin of any language uses
//! to call a Session's or a line's data face by id
//! ([#2127](https://github.com/arcships/dim/issues/2127)); `check` fails when
//! that file is stale.
use std::path::{Path, PathBuf};

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

fn file(root: &Path) -> PathBuf {
    root.join("npm/branch-services.d.ts")
}

/// The names routing does not resolve, read from the runtime that refuses
/// them: its registration points and the two lifecycle services.
fn unroutable(root: &Path) -> Result<Vec<String>> {
    let path = root.join("crates/dim-agent-runtime/src/route.rs");
    let source = std::fs::read_to_string(&path)?;
    let ast = syn::parse_file(&source).map_err(|error| format!("{}: {error}", path.display()))?;
    let mut names = vec!["sessionLifecycle".to_owned(), "branchLifecycle".to_owned()];
    for item in &ast.items {
        let syn::Item::Const(declaration) = item else {
            continue;
        };
        if declaration.ident != "REGISTRATION_POINTS" {
            continue;
        }
        if let syn::Expr::Array(array) = &*declaration.expr {
            for element in &array.elems {
                if let syn::Expr::Lit(syn::ExprLit {
                    lit: syn::Lit::Str(name),
                    ..
                }) = element
                {
                    names.push(name.value());
                }
            }
        }
    }
    Ok(names)
}

/// The addresses over the two routable scopes: each member is a data face a
/// route resolves there, named as its catalog name, typed by the interface
/// this file already declares for it.
fn addresses(root: &Path, declarations: &str) -> Result<String> {
    let catalog =
        std::fs::read_to_string(root.join("docs/architecture/rust-agent/generated/catalog.md"))?;
    let unroutable = unroutable(root)?;
    let declared: Vec<&str> = declarations
        .lines()
        .filter_map(|line| {
            line.strip_prefix("export interface ")
                .or_else(|| line.strip_prefix("export type "))
                .and_then(|rest| rest.split([' ', '<', '{', '=']).next())
        })
        .collect();
    let mut session = Vec::new();
    let mut line = Vec::new();
    for row in catalog.lines().filter(|line| line.starts_with("| `")) {
        let cells: Vec<&str> = row.split('|').map(str::trim).collect();
        let Some(name) = cells.get(1).and_then(|name| name.strip_prefix('`')) else {
            continue;
        };
        let Some(name) = name.strip_suffix('`') else {
            continue;
        };
        let (group, face) = match (cells.get(2), cells.get(4)) {
            (Some(group), Some(face)) => (*group, *face),
            _ => continue,
        };
        if face != "data" || name == "route" || name == "lineRoute" {
            continue;
        }
        if unroutable.iter().any(|refused| refused == name) {
            continue;
        }
        let Some(ty) = declared.iter().find(|ty| catalog_name(ty) == name).copied() else {
            continue;
        };
        match group {
            "session" => session.push((name.to_owned(), ty.to_owned())),
            "branch" => line.push((name.to_owned(), ty.to_owned())),
            _ => {}
        }
    }
    let mut text = String::from(
        "\n/**\n * A Session's data faces, addressed by id: nothing is looked up until a method of\n * one is called, and each call looks the target up again.\n */\nexport interface SessionAddress {\n  /** This Session's line `lineId`. */\n  line(lineId: string): LineAddress\n",
    );
    for (name, ty) in &session {
        text += &format!("  {name}: {ty}\n");
    }
    text += "}\n\n/** A line's data faces, addressed by id. */\nexport interface LineAddress {\n";
    for (name, ty) in &line {
        text += &format!("  {name}: {ty}\n");
    }
    text += "}\n\n/** Addresses Session `sessionId`; every call looks it up again. */\nexport declare function session(sessionId: string): SessionAddress\n\n/** Addresses line `lineId` of this Session; every call looks it up again. */\nexport declare function line(lineId: string): LineAddress\n";
    Ok(text)
}

/// The catalog name of a Rust type: its type name with the leading word
/// lowercased, as `#[rutis::service]` names a type (`InputQueue` is
/// `inputQueue`).
fn catalog_name(type_name: &str) -> String {
    let ident = type_name
        .trim_start_matches("dyn ")
        .split(['<', ' '])
        .next()
        .unwrap_or(type_name);
    let ident = ident.rsplit("::").next().unwrap_or(ident);
    let chars: Vec<char> = ident.chars().collect();
    let upper = chars.iter().take_while(|c| c.is_uppercase()).count();
    let lower = if upper > 1 && upper < chars.len() && chars[upper].is_lowercase() {
        upper - 1
    } else {
        upper.max(1).min(chars.len())
    };
    chars
        .iter()
        .enumerate()
        .flat_map(|(index, c)| {
            if index < lower {
                c.to_lowercase().collect::<Vec<_>>()
            } else {
                vec![*c]
            }
        })
        .collect()
}

fn declarations(root: &Path) -> Result<String> {
    let services = rutis_types::Exports::new()
        .declared("serde_json::Value", "import('./plugin.js').BridgeJsonValue")
        .declared("serde_json::Map", "import('./plugin.js').BridgeJsonObject")
        .declared(
            "dim_agent_types::PermissionPolicy",
            "import('./plugin.js').PermissionPolicy",
        )
        .declared(
            "dim_agent_types::AgentEvent",
            "import('./plugin.js').BranchEvent",
        )
        .declared(
            "dim_agent_types::EventDraft",
            "{ type: string; name: string; data: unknown; tags?: string[]; context_render?: boolean; context_state?: boolean }",
        )
        .declared(
            "dim_agent_runtime::TimelineIdentity",
            "import('./plugin.js').SessionRef",
        )
        .declared(
            "dim_agent_runtime::SessionIdentity",
            "{ sessionId: string; restored: boolean }",
        )
        .declared(
            "dim_agent_runtime::SessionConfig",
            "import('./plugin.js').SessionConfig",
        )
        .declared(
            "dim_agent_types::ModelConfig",
            "import('./plugin.js').ModelConfig",
        )
        .declared("std::num::NonZeroU64", "number | bigint")
        .declared(
            "dim_agent_types::ToolCall",
            "{ id: string; name: string; input: { [key: string]: unknown } }",
        )
        .declared(
            "dim_agent_types::ToolResult",
            "import('./plugin.js').ToolResult",
        )
        .declared(
            "dim_agent_types::TimelinePosition",
            "{ branch_id: string; sequence: number | bigint }",
        )
        .declarations(root.join("crates/dim-app-server"))?;
    let addresses = addresses(root, &services)?;
    Ok(services + &addresses)
}

pub fn write(root: &Path) -> Result<()> {
    std::fs::write(file(root), declarations(root)?)?;
    Ok(())
}

pub fn check(root: &Path) -> Result<()> {
    let path = file(root);
    let written = std::fs::read_to_string(&path).unwrap_or_default();
    if written != declarations(root)? {
        return Err(format!(
            "{}: service declarations are stale; run `cargo run -p xtask -- types`",
            path.display()
        )
        .into());
    }
    println!("service declarations passed");
    Ok(())
}
