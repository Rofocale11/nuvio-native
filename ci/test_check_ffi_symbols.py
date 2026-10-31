#!/usr/bin/env python3
"""ci/check-ffi-symbols.py's verdicts on synthetic trees, plus the real tree (read-only) and its wiring.

Each case is a private temp tree with a `src/` of C files and a `rust-modules/` of Rust ones. The
real-tree cases read the repository, and the wiring cases read ci/check-python-steps.txt, because a
gate that no step names is a gate nobody runs (`ci/test_ci_split.py`).

The bug it was written for: `src/main.c` called `plx_launch_args`, the merge resolution kept the C
declaration and dropped the `#[no_mangle]` export, and the ARM link in GitHub Actions was the first
thing to notice — no host step links C against Rust."""
import importlib.util
import tempfile
import unittest
from pathlib import Path

_spec = importlib.util.spec_from_file_location('check_ffi_symbols', Path(__file__).with_name('check-ffi-symbols.py'))
gate = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(gate)

REPO = Path(__file__).resolve().parent.parent
MANIFEST = REPO / 'ci' / 'check-python-steps.txt'

MAIN_C = 'extern int foo(const char *x);\n\nint main(int argc, char **argv) {\n    return foo(argv[0]);\n}\n'


def tree(c=None, headers=None, rust=None):
    """Problems for a private tree of C sources and Rust exports."""
    with tempfile.TemporaryDirectory(prefix='check-ffi-symbols-') as tmp:
        root = Path(tmp)
        (root / 'src').mkdir()
        (root / 'rust-modules').mkdir()
        for name, text in (c or {}).items():
            (root / 'src' / name).write_text(text)
        for name, text in (headers or {}).items():
            (root / 'src' / name).write_text(text)
        for name, text in (rust or {}).items():
            path = root / 'rust-modules' / name
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_text(text)
        return gate.problems(root)


class Resolves(unittest.TestCase):
    def test_a_rust_export_satisfies_the_extern(self):
        self.assertEqual(tree({'main.c': MAIN_C}, rust={
            'lib.rs': '#[no_mangle]\npub extern "C" fn foo(x: *const u8) -> i32 {\n    0\n}\n'}), [])

    def test_a_definition_in_another_c_file_satisfies_the_extern(self):
        self.assertEqual(tree({'main.c': MAIN_C, 'foo.c': 'int foo(const char *x) { (void)x; return 0; }\n'}), [])

    def test_a_header_declaration_is_read_the_same_way(self):
        self.assertEqual(tree({'main.c': '#include "foo.h"\nint main(void) { return foo(); }\n'},
                              headers={'foo.h': 'extern int foo(void);\n'},
                              rust={'a.rs': '#[unsafe(no_mangle)]\npub unsafe extern "C" fn foo() -> i32 { 0 }\n'}), [])

    def test_export_name_is_a_name(self):
        self.assertEqual(tree({'main.c': MAIN_C}, rust={
            'a.rs': '#[export_name = "foo"]\npub extern "C" fn renamed() -> i32 { 0 }\n'}), [])

    def test_a_firmware_asm_label_is_not_ours_to_define(self):
        self.assertEqual(tree({'main.c': 'extern void SMP_Play(void *self) __asm__("_ZN17StarfishMediaAPIs4PlayEv");\n'
                                         'int main(void) { SMP_Play(0); return 0; }\n'}), [])

    def test_an_extern_variable_is_not_a_function(self):
        self.assertEqual(tree({'main.c': 'extern void *elogf;\nint main(void) { return elogf == 0; }\n'}), [])


class Reports(unittest.TestCase):
    def test_a_dropped_export_is_the_whole_point(self):
        found = tree({'main.c': MAIN_C})
        self.assertEqual(len(found), 1)
        self.assertIn('src/main.c:1', found[0])
        self.assertIn('foo', found[0])

    def test_a_static_definition_is_private_to_its_file(self):
        found = tree({'main.c': MAIN_C, 'foo.c': 'static int foo(const char *x) { (void)x; return 0; }\n'})
        self.assertEqual(len(found), 1, found)
        self.assertIn('foo', found[0])

    def test_an_export_inside_test_code_does_not_count(self):
        found = tree({'main.c': MAIN_C}, rust={'a.rs': '#[cfg(test)]\nmod tests {\n    #[no_mangle]\n'
                                                        '    pub extern "C" fn foo() {}\n}\n'})
        self.assertEqual(len(found), 1, found)
        self.assertIn('foo', found[0])


class RealTree(unittest.TestCase):
    def test_the_repository_resolves_every_extern(self):
        self.assertEqual(gate.problems(REPO), [])

    def test_the_gate_is_a_step_of_make_check(self):
        self.assertIn('python3 ci/check-ffi-symbols.py\n', MANIFEST.read_text(),
                      'ci/check-python-steps.txt must name the gate')

    def test_this_suite_is_a_step_of_make_check(self):
        self.assertIn('python3 ci/test_check_ffi_symbols.py\n', MANIFEST.read_text(),
                      'ci/check-python-steps.txt must name this suite')


if __name__ == '__main__':
    unittest.main()
