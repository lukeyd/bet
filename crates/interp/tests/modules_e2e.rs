//! End-to-end: multi-file `bet` programs resolved by `frontend::load` and executed on the
//! interpreter. Proves the resolve-and-mangle pass produces a runnable single program — a
//! namespace-qualified call across files behaves exactly like an in-file call.

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

/// A throwaway directory holding one test's `.bet` files; removed on drop.
struct Fixture {
    dir: PathBuf,
}

impl Fixture {
    fn new(files: &[(&str, &str)]) -> Fixture {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!("bet_mod_e2e_{}_{n}", std::process::id()));
        for (rel, src) in files {
            let path = dir.join(rel);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(&path, src).unwrap();
        }
        Fixture { dir }
    }

    fn run(&self, entry: &str) -> Result<String, String> {
        let program = frontend::load(&self.dir.join(entry)).map_err(|e| e.to_string())?;
        interp::run_to_string(&program).map_err(|e| e.to_string())
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

#[test]
fn qualified_calls_and_consts_across_files() {
    let fx = Fixture::new(&[
        (
            "main.bet",
            "pull \"geometry\"\n\
             finna main() {\n\
             \x20   spill.it(geometry.area(3, 4))\n\
             \x20   spill.it(geometry.PI)\n\
             \x20   spill.it(local())\n\
             }\n\
             finna local() -> int {\n\
             \x20   bet geometry.area(2, 2)\n\
             }\n",
        ),
        (
            "geometry.bet",
            "flex facts PI: int = 3\n\
             flex finna area(w: int, h: int) -> int {\n\
             \x20   bet w * h\n\
             }\n",
        ),
    ]);
    assert_eq!(fx.run("main.bet").unwrap(), "12\n3\n4\n");
}

#[test]
fn subdir_import_with_alias() {
    let fx = Fixture::new(&[
        (
            "main.bet",
            "pull \"shapes/geometry\" as geo\n\
             finna main() {\n\
             \x20   spill.it(geo.area(5, 6))\n\
             }\n",
        ),
        (
            "shapes/geometry.bet",
            "flex finna area(w: int, h: int) -> int {\n\
             \x20   bet w * h\n\
             }\n",
        ),
    ]);
    assert_eq!(fx.run("main.bet").unwrap(), "30\n");
}

#[test]
fn diamond_import_runs_once() {
    // main pulls a and b; both pull common. `common.tick()` returns a constant; the shared file
    // must contribute exactly one definition (no duplicate-symbol blowup).
    let fx = Fixture::new(&[
        (
            "main.bet",
            "pull \"a\"\npull \"b\"\n\
             finna main() {\n\
             \x20   spill.it(a.ay() + b.be())\n\
             }\n",
        ),
        (
            "a.bet",
            "pull \"common\"\nflex finna ay() -> int {\n bet common.base() + 1\n}\n",
        ),
        (
            "b.bet",
            "pull \"common\"\nflex finna be() -> int {\n bet common.base() + 2\n}\n",
        ),
        ("common.bet", "flex finna base() -> int {\n bet 10\n}\n"),
    ]);
    // (10+1) + (10+2) = 23
    assert_eq!(fx.run("main.bet").unwrap(), "23\n");
}

#[test]
fn qualified_types_across_files() {
    // A `drip` exported from geometry, referenced as `geometry.Point` in type position, and a
    // function taking/returning it. Bare struct literals inside geometry mangle consistently.
    let fx = Fixture::new(&[
        (
            "main.bet",
            "pull \"geometry\"\n\
             finna main() {\n\
             \x20   lowkey p: geometry.Point = geometry.origin()\n\
             \x20   spill.it(geometry.sumxy(p))\n\
             }\n",
        ),
        (
            "geometry.bet",
            "flex drip Point {\n\
             \x20   flex x: int\n\
             \x20   flex y: int\n\
             }\n\
             flex finna origin() -> Point {\n\
             \x20   bet Point{ x: 3, y: 4 }\n\
             }\n\
             flex finna sumxy(p: Point) -> int {\n\
             \x20   bet p.x + p.y\n\
             }\n",
        ),
    ]);
    assert_eq!(fx.run("main.bet").unwrap(), "7\n");
}

#[test]
fn qualified_struct_literals_variants_and_patterns() {
    // Exercises: a qualified struct literal `geometry.Point{..}`; a cross-module variant
    // constructor `geometry.Circle(5)` fed to an own-module `vibe`; and a qualified variant
    // pattern `geometry.Circle(r)` in a `vibe` written in the root file.
    let fx = Fixture::new(&[
        (
            "main.bet",
            "pull \"geometry\"\n\
             finna main() {\n\
             \x20   lowkey p: geometry.Point = geometry.Point{ x: 3, y: 4 }\n\
             \x20   spill.it(p.x + p.y)\n\
             \x20   spill.it(geometry.measure(geometry.Circle(5)))\n\
             \x20   spill.it(geometry.measure(geometry.Rect(2, 6)))\n\
             \x20   spill.it(here(geometry.Circle(9)))\n\
             }\n\
             finna here(s: geometry.Shape) -> int {\n\
             \x20   vibe s {\n\
             \x20       geometry.Circle(r) { bet r + 1 }\n\
             \x20       geometry.Rect(w, h) { bet w + h }\n\
             \x20   }\n\
             }\n",
        ),
        (
            "geometry.bet",
            "flex drip Point {\n\
             \x20   flex x: int\n\
             \x20   flex y: int\n\
             }\n\
             flex moods Shape {\n\
             \x20   Circle(int),\n\
             \x20   Rect(int, int),\n\
             }\n\
             flex finna measure(s: Shape) -> int {\n\
             \x20   vibe s {\n\
             \x20       Circle(r) { bet r * r }\n\
             \x20       Rect(w, h) { bet w * h }\n\
             \x20   }\n\
             }\n",
        ),
    ]);
    // 7, 25 (5*5), 12 (2*6), 10 (9+1)
    assert_eq!(fx.run("main.bet").unwrap(), "7\n25\n12\n10\n");
}

#[test]
fn hush_across_files_is_rejected() {
    let fx = Fixture::new(&[
        (
            "main.bet",
            "pull \"geometry\"\nfinna main() {\n spill.it(geometry.secret())\n}\n",
        ),
        ("geometry.bet", "finna secret() -> int {\n bet 9\n}\n"),
    ]);
    let err = fx.run("main.bet").unwrap_err();
    assert!(
        err.contains("hush"),
        "expected a visibility error, got: {err}"
    );
}
