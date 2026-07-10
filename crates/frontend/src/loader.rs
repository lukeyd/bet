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
    let entry = normalize(entry);
    let entry_dir = entry
        .parent()
        .unwrap_or_else(|| Path::new(""))
        .to_path_buf();
    // Confine every import under a project root so `pull` can't read files outside it. The root is
    // the build's working directory when the entry lives inside it — the project/sandbox boundary,
    // which lets multi-directory projects import siblings (`../defs`, `../p`) while still rejecting
    // climbs to arbitrary host paths and absolute imports. It falls back to the entry's own
    // directory for an entry given outside CWD, and `root_canonical` is `None` for the in-memory
    // test reader (fictional paths), where `root_lexical` is the confinement floor instead.
    let cwd = std::env::current_dir()
        .ok()
        .and_then(|c| c.canonicalize().ok());
    let root_canonical = import_root(&entry, &entry_dir, cwd.as_deref());
    let root_lexical = normalize(&entry_dir);
    // The root's user-facing name: its own file name, never its absolute directory.
    let display = entry
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| entry.display().to_string());
    let mut b = GraphBuilder {
        reader,
        modules: Vec::new(),
        loaded: HashMap::new(),
        stack: Vec::new(),
        root_lexical,
        root_canonical,
        cwd,
    };
    b.load_file(&entry, &display, true)?;
    Ok(ModuleGraph { modules: b.modules })
}

struct GraphBuilder<'r, R> {
    reader: &'r mut R,
    modules: Vec<LoadedModule>,
    /// Normalized path → finalized module index (dedup: each file loads once).
    loaded: HashMap<PathBuf, usize>,
    /// The active DFS path — path plus its user-facing display name — for cycle detection and
    /// a readable, leak-free cycle message.
    stack: Vec<(PathBuf, String)>,
    /// Lexically-normalized floor used only when `root_canonical` is `None` (the in-memory reader).
    root_lexical: PathBuf,
    /// Canonicalized project root (CWD, or the entry's dir for an out-of-tree entry) when the entry
    /// is a real file; resolved-target confinement checks against it. `None` for the in-memory reader.
    root_canonical: Option<PathBuf>,
    /// Canonicalized working directory, used to absolutize a relative import target that doesn't
    /// exist yet (a missing import) so its confinement can still be judged. `None` if unavailable.
    cwd: Option<PathBuf>,
}

impl<R> GraphBuilder<'_, R>
where
    R: FnMut(&Path) -> std::io::Result<String>,
{
    fn load_file(
        &mut self,
        path: &Path,
        display: &str,
        is_root: bool,
    ) -> Result<usize, CompileError> {
        if let Some(&idx) = self.loaded.get(path) {
            return Ok(idx); // already fully loaded — diamond import, load once
        }
        if self.stack.iter().any(|(p, _)| p == path) {
            let mut chain: Vec<String> = self.stack.iter().map(|(_, d)| d.clone()).collect();
            chain.push(display.to_string());
            return Err(CompileError::Load(format!(
                "import cycle: {}",
                chain.join(" -> ")
            )));
        }

        // Errors past this point name the module the way the user wrote it (`display`), never the
        // resolved absolute path — so a failed read can't be used to probe the filesystem.
        let src = (self.reader)(path)
            .map_err(|e| CompileError::Load(format!("reading \"{display}\": {e}")))?;
        let program =
            parse(&src).map_err(|e| CompileError::Load(format!("in \"{display}\": {e}")))?;

        self.stack.push((path.to_path_buf(), display.to_string()));
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
                    "import \"{}\" binds namespace `{name}`, which is a built-in module \
                     name — add an `as` alias",
                    p.module
                )));
            }
            if imports.iter().any(|i| i.name == name) {
                return Err(CompileError::Load(format!(
                    "namespace `{name}` is imported twice — add an `as` alias"
                )));
            }
            // Reject an absolute import string outright, then confine the resolved target under the
            // project root — `confined` is what bounds `..` hops: a `..` that stays inside the root
            // (e.g. `sub/mod` pulling `../state`) is fine, one that climbs out is rejected below,
            // before any file read.
            reject_absolute(&p.module)?;
            let target_display = format!("{}.bet", p.module);
            let target_path = normalize(&parent.join(&target_display));
            if !self.confined(&target_path) {
                return Err(CompileError::Load(format!(
                    "pull \"{}\" escapes the project root — imports gotta stay inside the \
                     project, no cap",
                    p.module
                )));
            }
            let target = self.load_file(&target_path, &target_display, false)?;
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

    /// Is `target` confined under the project root? On a real filesystem the target is resolved to
    /// an absolute path — canonicalized when it exists (so a symlinked escape resolves outside the
    /// root and is rejected), else absolutized lexically (so a not-yet-existing in-root import still
    /// validates) — and required to start with the root. For the in-memory reader (no `root_canonical`)
    /// lexical containment under `root_lexical` is the check.
    fn confined(&self, target: &Path) -> bool {
        match &self.root_canonical {
            Some(root) => {
                let abs = target
                    .canonicalize()
                    .unwrap_or_else(|_| self.absolutize(target));
                abs.starts_with(root)
            }
            None => normalize(target).starts_with(&self.root_lexical),
        }
    }

    /// Resolve a (possibly relative, possibly missing) target to a lexical absolute path: an
    /// absolute target is normalized in place; a relative one is joined onto the working directory.
    fn absolutize(&self, target: &Path) -> PathBuf {
        if target.is_absolute() {
            normalize(target)
        } else if let Some(cwd) = &self.cwd {
            normalize(&cwd.join(target))
        } else {
            normalize(target)
        }
    }
}

/// Choose the import-confinement root: the canonical working directory when the entry resolves
/// inside it (the project/sandbox boundary — allows in-project sibling imports like `../defs`),
/// otherwise the entry's own directory (for an entry passed as a path outside CWD). Returns `None`
/// when nothing resolves on disk (the in-memory test reader), where a lexical floor is used instead.
fn import_root(entry: &Path, entry_dir: &Path, cwd: Option<&Path>) -> Option<PathBuf> {
    if let Some(cwd) = cwd
        && let Ok(entry_abs) = entry.canonicalize()
        && entry_abs.starts_with(cwd)
    {
        return Some(cwd.to_path_buf());
    }
    entry_dir.canonicalize().ok()
}

/// Reject an absolute `pull` target (a leading `/` or drive prefix) — imports must be relative so
/// they resolve against the importing file and stay inside the project. `..` hops are *not* rejected
/// here: whether a `..` climbs out of the root is decided by `confined` on the resolved path, which
/// correctly allows an in-root sibling hop (`../state`) while rejecting an escape. Reports only the
/// module string as the user wrote it, so the message can't leak a resolved filesystem path.
fn reject_absolute(module: &str) -> Result<(), CompileError> {
    for comp in Path::new(module).components() {
        if matches!(comp, Component::RootDir | Component::Prefix(_)) {
            return Err(CompileError::Load(format!(
                "pull \"{module}\" is an absolute path — keep imports relative and inside \
                 the project, fr"
            )));
        }
    }
    Ok(())
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
            // A `..` pops a preceding normal segment; at a root/prefix, or with nothing (or only
            // `..`) accumulated, it can only escape — clamp it instead of retaining a leading `..`.
            Component::ParentDir => {
                if let Some(Component::Normal(_)) = out.components().next_back() {
                    out.pop();
                }
            }
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

    #[test]
    fn normalize_clamps_leading_parent_dirs() {
        // A leading `..` must not survive — otherwise it escapes the confinement root.
        assert_eq!(
            normalize(Path::new("../../etc/passwd")),
            PathBuf::from("etc/passwd")
        );
        assert_eq!(normalize(Path::new("a/../../b")), PathBuf::from("b"));
    }

    #[test]
    fn parent_traversal_import_is_rejected() {
        // A `..` chain that climbs out of the project root must be rejected (issue #39).
        let err = graph(
            "proj/main.bet",
            &[(
                "proj/main.bet",
                "pull \"../../etc/hostname\"\nfinna main() {}\n",
            )],
        )
        .unwrap_err();
        let CompileError::Load(m) = err else {
            panic!("expected a Load error, got {err:?}")
        };
        assert!(m.contains(".."), "should call out the `..` climb: {m}");
    }

    #[test]
    fn absolute_import_is_rejected() {
        // `pull "/etc/hostname"` must be rejected as an absolute path (issue #39).
        let err = graph(
            "main.bet",
            &[("main.bet", "pull \"/etc/hostname\"\nfinna main() {}\n")],
        )
        .unwrap_err();
        let CompileError::Load(m) = err else {
            panic!("expected a Load error, got {err:?}")
        };
        assert!(
            m.contains("absolute"),
            "should call out the absolute path: {m}"
        );
    }

    #[test]
    fn legit_relative_import_still_loads_after_confinement() {
        // Confinement must not break ordinary sibling / subdir imports.
        let g = graph(
            "main.bet",
            &[
                (
                    "main.bet",
                    "pull \"geometry\"\npull \"sub/thing\" as x\nfinna main() {}\n",
                ),
                ("geometry.bet", "flex finna area() {}\n"),
                ("sub/thing.bet", "flex finna helper() {}\n"),
            ],
        )
        .unwrap();
        assert_eq!(g.modules.len(), 3);
        let names: Vec<&str> = root(&g).imports.iter().map(|i| i.name.as_str()).collect();
        assert_eq!(names, ["geometry", "x"]);
    }

    #[test]
    fn in_root_parent_hop_is_allowed() {
        // A `..` that stays inside the project root must load (regression: the confinement fix
        // must not blanket-reject `..`). Mirrors corpus `12-doom/gamestate-crib`: a subdir module
        // pulls `../state`, resolving to a sibling of the entry — still under the root.
        let g = graph(
            "proj/main.bet",
            &[
                ("proj/main.bet", "pull \"sub/mod\" as m\nfinna main() {}\n"),
                ("proj/sub/mod.bet", "pull \"../state\"\nflex finna f() {}\n"),
                ("proj/state.bet", "flex finna g() {}\n"),
            ],
        )
        .unwrap();
        assert!(
            g.modules.iter().any(|m| m.stem == "state"),
            "../state should resolve to the in-root sibling and load"
        );
    }

    #[test]
    fn missing_import_error_hides_absolute_path() {
        // A failed read reports the import as written, not the resolved absolute path — so the
        // error can't be used as a file-existence oracle for the project's location on disk.
        let err = graph(
            "/secret/project/main.bet",
            &[(
                "/secret/project/main.bet",
                "pull \"typo\"\nfinna main() {}\n",
            )],
        )
        .unwrap_err();
        let CompileError::Load(m) = err else {
            panic!("expected a Load error, got {err:?}")
        };
        assert!(m.contains("typo.bet"), "should name the import: {m}");
        assert!(
            !m.contains("/secret/project"),
            "must not leak the absolute project path: {m}"
        );
    }
}
