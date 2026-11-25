//! The access-layer rules of [#2015 §5](https://github.com/arcships/dim/issues/2015)
//! and the list that admits the violations still in the tree.
//!
//! A violation is one of: `#[service(skip)]` on a service method, a
//! hand-written `HostDispatch` implementation, a `Session` method that takes a
//! line's name, and the outward `Branch` type. The scan reads the product
//! sources (`crates/*/src`) only: a test fixture is not a public face.
//!
//! The allowlist (`xtask/access-layer.json`) names every violation the tree
//! still has. `check` fails when a violation is not listed and when a listed
//! entry no longer matches anything, so a change can only remove entries, never
//! add one. The same file's `removed` list names catalog services that a change
//! deleted; the runtime catalog check in `xtask` fails when one comes back.
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeSet,
    path::{Path, PathBuf},
};

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

/// The file holding the allowlist and the removed names, from the workspace
/// root.
pub const FILE: &str = "xtask/access-layer.json";

/// What kind of access-layer violation a rule names.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Kind {
    /// A service method kept out of the face with `#[service(skip)]`.
    ServiceSkip,
    /// A `HostDispatch` implementation written by hand instead of a
    /// `#[rutis::service]` block.
    HostDispatch,
    /// A `Session` method that forwards to a line named by an argument.
    SessionForwarding,
    /// The outward `Branch` type.
    BranchType,
}

/// One violation, identified by what it is and where it lives, not by line
/// numbers.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Rule {
    pub kind: Kind,
    /// The source file, from the workspace root.
    pub file: String,
    /// The item: `Type::method` for a method, the type for a type.
    pub item: String,
}

impl std::fmt::Display for Rule {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {} {}", self.file, self.item, kind_name(self.kind))
    }
}

fn kind_name(kind: Kind) -> &'static str {
    match kind {
        Kind::ServiceSkip => "has #[service(skip)]",
        Kind::HostDispatch => "is a hand-written HostDispatch",
        Kind::SessionForwarding => "forwards to a line name",
        Kind::BranchType => "is an outward Branch type",
    }
}

/// The allowlist and the removed names.
#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AccessLayer {
    /// Violations the tree still has, one entry per occurrence.
    #[serde(default)]
    pub allow: Vec<Rule>,
    /// Catalog services a change deleted. Empty until the first face goes.
    #[serde(default)]
    pub removed: Vec<String>,
}

/// Reads the file at `root`; a missing file admits nothing.
pub fn load(root: &Path) -> Result<AccessLayer> {
    let path = root.join(FILE);
    match std::fs::read_to_string(&path) {
        Ok(text) => {
            Ok(serde_json::from_str(&text)
                .map_err(|error| format!("{}: {error}", path.display()))?)
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(AccessLayer::default()),
        Err(error) => Err(format!("{}: {error}", path.display()).into()),
    }
}

/// Fails when a violation is not allowed or an allowed entry matches nothing.
pub fn check(root: &Path) -> Result<()> {
    let found: BTreeSet<Rule> = violations(root)?.into_iter().collect();
    let layer = load(root)?;
    let allowed: BTreeSet<Rule> = layer.allow.iter().cloned().collect();
    if allowed.len() != layer.allow.len() {
        let mut seen = BTreeSet::new();
        let duplicates: Vec<_> = layer
            .allow
            .iter()
            .filter(|rule| !seen.insert((*rule).clone()))
            .collect();
        return Err(format!("{}: listed twice: {duplicates:?}", FILE).into());
    }
    let unlisted: Vec<_> = found.difference(&allowed).collect();
    if !unlisted.is_empty() {
        let mut message = format!(
            "{} access-layer violation(s) are not in {FILE}; the list may only shrink:\n",
            unlisted.len()
        );
        for rule in unlisted {
            message += &format!("{}\n", serde_json::to_string(rule).expect("a rule is JSON"));
        }
        return Err(message.into());
    }
    let stale: Vec<_> = allowed.difference(&found).collect();
    if !stale.is_empty() {
        let mut message = format!(
            "{} entries of {FILE} match nothing and must be deleted:\n",
            stale.len()
        );
        for rule in stale {
            message += &format!("{rule}\n");
        }
        return Err(message.into());
    }
    println!("access layer passed ({} allowed)", allowed.len());
    Ok(())
}

/// Every violation in the product sources under `root`.
pub fn violations(root: &Path) -> Result<Vec<Rule>> {
    let mut found = Vec::new();
    for file in sources(root)? {
        let text = std::fs::read_to_string(&file)?;
        let parsed =
            syn::parse_file(&text).map_err(|error| format!("{}: {error}", file.display()))?;
        let relative = file
            .strip_prefix(root)
            .unwrap_or(&file)
            .to_string_lossy()
            .replace('\\', "/");
        scan(&parsed.items, &relative, &mut found);
    }
    found.sort();
    Ok(found)
}

/// The product source files: every `.rs` under `crates/*/src`.
fn sources(root: &Path) -> Result<Vec<PathBuf>> {
    let mut files = Vec::new();
    let crates = root.join("crates");
    for entry in std::fs::read_dir(&crates)? {
        let directory = entry?.path().join("src");
        if directory.is_dir() {
            collect(&directory, &mut files)?;
        }
    }
    files.sort();
    Ok(files)
}

fn collect(directory: &Path, files: &mut Vec<PathBuf>) -> Result<()> {
    for entry in std::fs::read_dir(directory)? {
        let path = entry?.path();
        if path.is_dir() {
            collect(&path, files)?;
        } else if path.extension().is_some_and(|extension| extension == "rs") {
            files.push(path);
        }
    }
    Ok(())
}

fn scan(items: &[syn::Item], file: &str, found: &mut Vec<Rule>) {
    for item in items {
        let syn::Item::Impl(block) = item else {
            if let syn::Item::Mod(module) = item
                && let Some((_, items)) = &module.content
            {
                scan(items, file, found);
            }
            continue;
        };
        let service = block.attrs.iter().any(is_service_attribute);
        for method in &block.items {
            let syn::ImplItem::Fn(method) = method else {
                continue;
            };
            if service
                && method
                    .attrs
                    .iter()
                    .any(|attr| is_service_attribute(attr) && skips(attr))
            {
                found.push(Rule {
                    kind: Kind::ServiceSkip,
                    file: file.to_owned(),
                    item: format!("{}::{}", type_name(&block.self_ty), method.sig.ident),
                });
            }
        }
        if block
            .trait_
            .as_ref()
            .and_then(|(_, path, _)| path.segments.last())
            .is_some_and(|segment| segment.ident == "HostDispatch")
        {
            found.push(Rule {
                kind: Kind::HostDispatch,
                file: file.to_owned(),
                item: type_name(&block.self_ty),
            });
        }
        if block.trait_.is_none() && type_name(&block.self_ty) == "Session" {
            for method in &block.items {
                let syn::ImplItem::Fn(method) = method else {
                    continue;
                };
                let named_branch = method.sig.inputs.iter().any(|input| {
                    matches!(input, syn::FnArg::Typed(typed)
                        if matches!(&*typed.pat, syn::Pat::Ident(ident) if ident.ident == "branch"))
                });
                if named_branch {
                    found.push(Rule {
                        kind: Kind::SessionForwarding,
                        file: file.to_owned(),
                        item: format!("Session::{}", method.sig.ident),
                    });
                }
            }
        }
    }
    for item in items {
        let (visibility, ty) = match item {
            syn::Item::Struct(item) => (&item.vis, &item.ident),
            syn::Item::Enum(item) => (&item.vis, &item.ident),
            _ => continue,
        };
        if matches!(visibility, syn::Visibility::Public(_)) && ty == "Branch" {
            found.push(Rule {
                kind: Kind::BranchType,
                file: file.to_owned(),
                item: ty.to_string(),
            });
        }
    }
}

/// `#[service]`, `#[service(skip)]` or their `rutis::` spellings.
fn is_service_attribute(attr: &syn::Attribute) -> bool {
    path_is_service(attr.path())
}

fn path_is_service(path: &syn::Path) -> bool {
    let segments: Vec<String> = path
        .segments
        .iter()
        .map(|segment| segment.ident.to_string())
        .collect();
    matches!(segments.as_slice(), [service] if service == "service")
        || matches!(segments.as_slice(), [root, service] if root == "rutis" && service == "service")
}

fn skips(attr: &syn::Attribute) -> bool {
    attr.parse_args::<syn::Ident>()
        .is_ok_and(|argument| argument == "skip")
}

/// The last path segment of a type, `Branch` for `crate::Branch`.
fn type_name(ty: &syn::Type) -> String {
    match ty {
        syn::Type::Path(path) => path
            .path
            .segments
            .last()
            .map(|segment| segment.ident.to_string())
            .unwrap_or_default(),
        _ => String::new(),
    }
}
