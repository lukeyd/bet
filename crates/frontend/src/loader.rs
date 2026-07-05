//! The module loader: turn a `.bet` entry file into a single merged [`ast::Program`] by
//! resolving its `pull` imports across files.
//!
//! `pull "geometry"` loads a sibling `geometry.bet` (relative to the importing file) and binds a
//! namespace; `pull "shapes/geometry" as geo` reaches into a subdirectory and renames the
//! namespace. Built-in stdlib module names (`spill`, `math`, … — see
//! [`crate::lower::is_builtin_module`]) are **not** files: `pull "spill"` stays an intrinsic
//! no-op exactly as before.
//!
//! The loader runs in two steps. First [`load_graph_with`] walks the import graph (reading and
//! parsing each file once, deduping shared files, detecting cycles). Then a resolve-and-mangle
//! pass (added incrementally — see the plan) rewrites every cross-file reference and concatenates
//! the modules into one `Program`, so the interpreter, lowering, and backend never learn that
//! modules exist. This module owns step one; step two arrives with later phases.

use crate::ast::{self, Item};
use crate::lower::is_builtin_module;
use crate::{CompileError, parse};
use std::collections::HashMap;
use std::path::{Component, Path, PathBuf};

/// Load `entry` and all files it transitively `pull`s into one merged [`ast::Program`].
///
/// Reads from the real filesystem. Errors ([`CompileError::Load`]) on a missing imported file,
/// an import cycle, or a namespace collision.
pub fn load(entry: &Path) -> Result<ast::Program, CompileError> {
    let graph = load_graph_with(entry, &mut |p| std::fs::read_to_string(p))?;
    crate::resolve::resolve(&graph)
}

/// One file loaded into the module graph.
#[derive(Debug)]
pub(crate) struct LoadedModule {
    /// Default namespace name: the file stem (`geometry.bet` → `geometry`).
    pub stem: String,
    /// The entry file. Its top-level names stay unmangled so `main` remains `main`.
    pub is_root: bool,
    /// Parsed items (still includes the `Pull` items).
    pub program: ast::Program,
    /// Resolved file imports, each a bound namespace name → index into [`ModuleGraph::modules`].
    pub imports: Vec<Import>,
}

/// A resolved `pull` of a source file: the namespace it binds and which module it points at.
#[derive(Debug)]
pub(crate) struct Import {
    /// The bound namespace name — the `as` alias if given, else the target's file stem.
    pub name: String,
    /// Index into [`ModuleGraph::modules`].
    pub target: usize,
}

/// The whole resolved import graph. `modules` is in a deterministic **post-order**: a file always
/// appears before the files that import it, and the root file is last — so concatenating items in
/// this order gives reproducible output and dense id assignment downstream.
#[derive(Debug)]
pub(crate) struct ModuleGraph {
    pub modules: Vec<LoadedModule>,
}

/// Build the import graph from `entry`, using `reader` to fetch each file's source (injected so
/// the graph logic is unit-testable with an in-memory file map).
pub(crate) fn load_graph_with<R>(entry: &Path, reader: &mut R) -> Result<ModuleGraph, CompileError>
where
    R: FnMut(&Path) -> std::io::Result<String>,
{
    let mut b = GraphBuilder {
        reader,
        modules: Vec::new(),
        loaded: HashMap::new(),
        stack: Vec::new(),
    };
    b.load_file(&normalize(entry), true)?;
    Ok(ModuleGraph { modules: b.modules })
}

struct GraphBuilder<'r, R> {
    reader: &'r mut R,
    modules: Vec<LoadedModule>,
    /// Normalized path → finalized module index (dedup: each file loads once).
    loaded: HashMap<PathBuf, usize>,
    /// The active DFS path, for cycle detection and a readable cycle message.
    stack: Vec<PathBuf>,
}

impl<R> GraphBuilder<'_, R>
where
    R: FnMut(&Path) -> std::io::Result<String>,
{
    fn load_file(&mut self, path: &Path, is_root: bool) -> Result<usize, CompileError> {
        if let Some(&idx) = self.loaded.get(path) {
            return Ok(idx); // already fully loaded — diamond import, load once
        }
        if self.stack.iter().any(|p| p == path) {
            let mut chain: Vec<String> =
                self.stack.iter().map(|p| p.display().to_string()).collect();
            chain.push(path.display().to_string());
            return Err(CompileError::Load(format!(
                "import cycle: {}",
                chain.join(" -> ")
            )));
        }

        let src = (self.reader)(path)
            .map_err(|e| CompileError::Load(format!("reading {}: {e}", path.display())))?;
        let program =
            parse(&src).map_err(|e| CompileError::Load(format!("in {}: {e}", path.display())))?;

        self.stack.push(path.to_path_buf());
        let parent = path.parent().unwrap_or_else(|| Path::new(""));
        let mut imports: Vec<Import> = Vec::new();
        for item in &program.items {
            let Item::Pull(p) = item else { continue };
            if is_builtin_module(&p.module) {
                continue; // a built-in stdlib module, not a file
            }
            let name = p
                .alias
                .clone()
                .unwrap_or_else(|| stem_of(Path::new(&p.module)));
            if is_builtin_module(&name) {
                return Err(CompileError::Load(format!(
                    "in {}: import \"{}\" binds namespace `{name}`, which is a built-in module \
                     name — add an `as` alias",
                    path.display(),
                    p.module
                )));
            }
            if imports.iter().any(|i| i.name == name) {
                return Err(CompileError::Load(format!(
                    "in {}: namespace `{name}` is imported twice — add an `as` alias",
                    path.display()
                )));
            }
            let target_path = normalize(&parent.join(format!("{}.bet", p.module)));
            let target = self.load_file(&target_path, false)?;
            imports.push(Import { name, target });
        }
        self.stack.pop();

        let idx = self.modules.len();
        self.modules.push(LoadedModule {
            stem: stem_of(path),
            is_root,
            program,
            imports,
        });
        self.loaded.insert(path.to_path_buf(), idx);
        Ok(idx)
    }
}

/// The file stem of a path or module string (`shapes/geometry` / `…/geometry.bet` → `geometry`).
fn stem_of(path: &Path) -> String {
    path.file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default()
}

/// Lexically normalize a path (resolve `.` and `..` without touching the filesystem) so different
/// spellings of the same file share a dedup key. No symlink resolution — deterministic and usable
/// with an in-memory reader.
fn normalize(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for comp in path.components() {
        match comp {
            Component::CurDir => {}
            Component::ParentDir => match out.components().next_back() {
                Some(Component::Normal(_)) => {
                    out.pop();
                }
                Some(Component::RootDir) | Some(Component::Prefix(_)) => {}
                _ => out.push(".."),
            },
            other => out.push(other.as_os_str()),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io;

    /// Build an in-memory reader over `(path, source)` pairs. Keys are normalized the same way the
    /// loader normalizes the paths it reads, so lookups line up.
    fn reader<'a>(files: &'a [(&'a str, &'a str)]) -> impl FnMut(&Path) -> io::Result<String> + 'a {
        let map: HashMap<PathBuf, String> = files
            .iter()
            .map(|(p, s)| (normalize(Path::new(p)), s.to_string()))
            .collect();
        move |p: &Path| {
            map.get(p)
                .cloned()
                .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "no such file"))
        }
    }

    fn graph(entry: &str, files: &[(&str, &str)]) -> Result<ModuleGraph, CompileError> {
        let mut r = reader(files);
        load_graph_with(Path::new(entry), &mut r)
    }

    fn root(g: &ModuleGraph) -> &LoadedModule {
        g.modules.iter().find(|m| m.is_root).unwrap()
    }

    #[test]
    fn single_file_no_imports() {
        let g = graph("main.bet", &[("main.bet", "finna main() {}\n")]).unwrap();
        assert_eq!(g.modules.len(), 1);
        assert_eq!(root(&g).stem, "main");
    }

    #[test]
    fn resolves_sibling_and_subdir() {
        let g = graph(
            "main.bet",
            &[
                (
                    "main.bet",
                    "pull \"geometry\"\npull \"shapes/extra\" as ex\nfinna main() {}\n",
                ),
                ("geometry.bet", "flex finna area() {}\n"),
                ("shapes/extra.bet", "flex finna helper() {}\n"),
            ],
        )
        .unwrap();
        // 3 modules; root is last (post-order); deps loaded before it.
        assert_eq!(g.modules.len(), 3);
        assert!(g.modules.last().unwrap().is_root);
        let names: Vec<&str> = root(&g).imports.iter().map(|i| i.name.as_str()).collect();
        assert_eq!(names, ["geometry", "ex"]);
    }

    #[test]
    fn diamond_loads_shared_file_once() {
        // main -> {a, b}; both a and b -> common. `common` must load exactly once.
        let g = graph(
            "main.bet",
            &[
                ("main.bet", "pull \"a\"\npull \"b\"\nfinna main() {}\n"),
                ("a.bet", "pull \"common\"\nflex finna fa() {}\n"),
                ("b.bet", "pull \"common\"\nflex finna fb() {}\n"),
                ("common.bet", "flex finna fc() {}\n"),
            ],
        )
        .unwrap();
        assert_eq!(g.modules.len(), 4, "common should appear once");
        // a and b import the SAME target index.
        let a = g.modules.iter().find(|m| m.stem == "a").unwrap();
        let b = g.modules.iter().find(|m| m.stem == "b").unwrap();
        assert_eq!(a.imports[0].target, b.imports[0].target);
    }

    #[test]
    fn cycle_is_an_error() {
        let err = graph(
            "main.bet",
            &[
                ("main.bet", "pull \"a\"\nfinna main() {}\n"),
                ("a.bet", "pull \"b\"\n"),
                ("b.bet", "pull \"a\"\n"),
            ],
        )
        .unwrap_err();
        assert!(matches!(err, CompileError::Load(m) if m.contains("import cycle")));
    }

    #[test]
    fn missing_file_is_an_error() {
        let err = graph(
            "main.bet",
            &[("main.bet", "pull \"nope\"\nfinna main() {}\n")],
        )
        .unwrap_err();
        assert!(matches!(err, CompileError::Load(m) if m.contains("nope.bet")));
    }

    #[test]
    fn builtin_module_is_not_a_file() {
        // `pull "spill"` must NOT try to read spill.bet.
        let g = graph(
            "main.bet",
            &[("main.bet", "pull \"spill\"\nfinna main() {}\n")],
        )
        .unwrap();
        assert_eq!(g.modules.len(), 1);
        assert!(root(&g).imports.is_empty());
    }

    #[test]
    fn same_stem_without_alias_collides() {
        let err = graph(
            "main.bet",
            &[
                ("main.bet", "pull \"a/geometry\"\npull \"b/geometry\"\n"),
                ("a/geometry.bet", "flex finna fa() {}\n"),
                ("b/geometry.bet", "flex finna fb() {}\n"),
            ],
        )
        .unwrap_err();
        assert!(matches!(err, CompileError::Load(m) if m.contains("imported twice")));
    }

    #[test]
    fn alias_avoids_stem_collision() {
        let g = graph(
            "main.bet",
            &[
                (
                    "main.bet",
                    "pull \"a/geometry\" as ag\npull \"b/geometry\" as bg\n",
                ),
                ("a/geometry.bet", "flex finna fa() {}\n"),
                ("b/geometry.bet", "flex finna fb() {}\n"),
            ],
        )
        .unwrap();
        let names: Vec<&str> = root(&g).imports.iter().map(|i| i.name.as_str()).collect();
        assert_eq!(names, ["ag", "bg"]);
    }

    #[test]
    fn alias_shadowing_builtin_is_an_error() {
        let err = graph(
            "main.bet",
            &[
                ("main.bet", "pull \"mymath\" as math\n"),
                ("mymath.bet", "flex finna f() {}\n"),
            ],
        )
        .unwrap_err();
        assert!(matches!(err, CompileError::Load(m) if m.contains("built-in module")));
    }

    #[test]
    fn normalize_resolves_dot_segments() {
        assert_eq!(normalize(Path::new("a/./b/../c")), PathBuf::from("a/c"));
        assert_eq!(normalize(Path::new("./x")), PathBuf::from("x"));
    }
}
