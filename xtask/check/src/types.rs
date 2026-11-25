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

/// The two data operations' requests, narrowed to what a route resolves: the
/// `service` is a data face of the target scope, `method` one of its methods,
/// and `args` the method's parameters, so a typo fails `tsc`
/// ([#2127](https://github.com/arcships/dim/issues/2127)).
fn calls(root: &Path, services: &str) -> Result<String> {
    let catalog =
        std::fs::read_to_string(root.join("docs/architecture/rust-agent/generated/catalog.md"))?;
    let unroutable = unroutable(root)?;
    let declared: Vec<&str> = services
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
        if face != "data" || name == "sessionScope" || name == "sessions" {
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
    let faces = |items: &[(String, String)], name: &str| {
        let mut text = format!("export type {name} = {{\n");
        for (name, ty) in items {
            text += &format!("  {name}: {ty}\n");
        }
        text + "}\n"
    };
    let mut text = String::from("\n/** The data faces a line reaches. */\n");
    text += &faces(&line, "LineFaces");
    text += "\n/** The data faces a Session reaches. */\n";
    text += &faces(&session, "SessionFaces");
    text += "\n/** One call a route resolves: `service` and `method` name a data face and one of its methods, and `args` are that method's parameters. */\ntype Call<Faces, Address> = {\n  [S in keyof Faces]: {\n    [M in keyof Faces[S]]: Address & {\n      service: S\n      method: M\n      args: Faces[S][M] extends (...args: infer A) => unknown ? A : never\n    }\n  }[keyof Faces[S]]\n}[keyof Faces]\n";
    text += "\n/** A call to one of the caller's own Session's lines. */\nexport type LineCall = Call<LineFaces, { lineId: string }>\n";
    text += "\n/** A call to one of the caller's own Session's data faces. */\nexport type SessionCall = Call<SessionFaces, {}>\n";
    text += "\n/** A call through Agents: a Session's face, or one of its lines' faces. */\nexport type SessionsCall =\n  | Call<LineFaces, { sessionId: string; lineId: string }>\n  | Call<SessionFaces, { sessionId: string }>\n";
    Ok(text)
}

/// The names routing does not resolve, read from the runtime that refuses
/// them: its registration points, plus the two lifecycle services. A missing
/// list is an error, never an empty one.
fn unroutable(root: &Path) -> Result<Vec<String>> {
    let path = root.join("crates/dim-agent-runtime/src/route.rs");
    let source = std::fs::read_to_string(&path)?;
    let ast = syn::parse_file(&source).map_err(|error| format!("{}: {error}", path.display()))?;
    let mut names = vec!["sessionLifecycle".to_owned(), "branchLifecycle".to_owned()];
    let mut found = false;
    for item in &ast.items {
        let syn::Item::Const(declaration) = item else {
            continue;
        };
        if declaration.ident != "REGISTRATION_POINTS" {
            continue;
        }
        let syn::Expr::Array(array) = &*declaration.expr else {
            return Err(format!("{}: REGISTRATION_POINTS is not an array", path.display()).into());
        };
        for element in &array.elems {
            let syn::Expr::Lit(syn::ExprLit {
                lit: syn::Lit::Str(name),
                ..
            }) = element
            else {
                return Err(format!(
                    "{}: REGISTRATION_POINTS holds an entry that is not a string",
                    path.display()
                )
                .into());
            };
            names.push(name.value());
        }
        found = true;
    }
    if !found {
        return Err(format!("{}: REGISTRATION_POINTS is missing", path.display()).into());
    }
    Ok(names)
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
    let calls = calls(root, &services)?;
    Ok(replaced(services) + &calls)
}

/// Drops the generated `LineCall`/`SessionCall` declarations, which the
/// narrowed ones replace.
fn replaced(services: String) -> String {
    let mut text = String::new();
    let mut skipping = false;
    for line in services.lines() {
        if line.starts_with("export type LineCall = {")
            || line.starts_with("export type SessionCall = {")
        {
            skipping = true;
            continue;
        }
        if skipping {
            if line == "}" {
                skipping = false;
            }
            continue;
        }
        if line == "/** A call to one of the caller's own Session's lines. */"
            || line == "/** A call to one of the caller's own Session's data faces. */"
        {
            continue;
        }
        text += line;
        text.push('\n');
    }
    text
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
