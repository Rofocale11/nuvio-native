#!/usr/bin/env python3
"""The C/Rust boundary gate: a function the C shim calls must be exported by the Rust side.

`src/main.c` is C and calls into the Rust application through `extern` declarations; `src/starfish.c`
does the same for the firmware's C++ symbols, which it names with `__asm__`. Nothing on this host
compiles `src/*.c` — the Makefile compiles `ci/private-log-test.c` for the log-file test and the
simulator builds `src/bin/sim.rs`, so the ONLY link that puts C and Rust in one binary is the ARM
`make ipk` in CI. A `#[no_mangle]` that a merge resolution dropped therefore costs a round trip
through GitHub Actions, which is exactly how `plx_launch_args` went missing on 2026-11-01:
`src/main.c` called it, `rust-modules/src/port.rs` no longer defined it, and `cargo check` (which
never links) was green.

This gate reads both sides without building:

  * every `extern` FUNCTION declaration in `src/*.c` and `src/*.h` (a declaration of a variable, or
    one carrying an `__asm__` label — a firmware symbol the C++ library provides — is not our
    side's to define);
  * every function DEFINITION in those C files (top level, not `static`, not `extern`);
  * every name the Rust tree exports: `#[no_mangle]`, `#[unsafe(no_mangle)]` (Rust 2024) or
    `#[export_name = "name"]` on a `fn`, anywhere under `rust-modules/`.

A C `extern` that neither list defines is a link failure waiting for the ARM build, and it is
reported with the file and line that declares it. The reverse direction is not a defect: Rust
exports exist for the firmware's library thread (`sf_on_event`, the Sentry spool entry) that no C
file in this tree calls.

    ci/check-ffi-symbols.py            # the gate (check-python-steps.txt runs it)
"""
from pathlib import Path
import importlib.util
import re
import sys

sys.path.insert(0, str(Path(__file__).resolve().parent))
import rust_test_modules  # noqa: E402

_spec = importlib.util.spec_from_file_location('check_plex_requests', Path(__file__).with_name('check-plex-requests.py'))
_plex = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(_plex)
without_test_items = _plex.without_test_items

REPO = Path(__file__).resolve().parent.parent

C_DIR = 'src'
RUST_DIR = 'rust-modules'

# `extern <type...> name(` — a function declaration. Trailing `__asm__("…")` labels are firmware
# symbols (StarfishMediaAPIs, ContentPipeline) and are not this repository's to define.
EXTERN_FN = re.compile(r'\bextern\b[^;{}()]*?\b(\w+)\s*\(', re.S)
ASM_LABEL = re.compile(r'__asm__\s*\(')
# A definition starts in column 0 with a type and a name; `static` defines for its own translation
# unit only, so it cannot satisfy another file's `extern`.
C_DEFINITION = re.compile(r'^([A-Za-z_][A-Za-z0-9_ \t\*]*?)\b(\w+)\s*\([^;{)]*\)\s*\{', re.M)


def declarations(path):
    """(name, line) for every `extern` function declaration in one C file, firmware labels excluded."""
    text = path.read_text(errors='replace')
    found = []
    for match in EXTERN_FN.finditer(text):
        line = text.count('\n', 0, match.start()) + 1
        # Look at the whole declaration up to its `;` for an asm label.
        end = text.find(';', match.end())
        statement = text[match.start():end if end != -1 else match.end()]
        if ASM_LABEL.search(statement):
            continue
        found.append((match.group(1), line))
    return found


def c_definitions(paths):
    names = set()
    for path in paths:
        for match in C_DEFINITION.finditer(path.read_text(errors='replace')):
            if match.group(1).strip().split()[:1] in (['static'], ['extern'], ['typedef']):
                continue
            names.add(match.group(2))
    return names


def exports_in(tokens):
    """The names `#[no_mangle]` / `#[unsafe(no_mangle)]` / `#[export_name = "…"]` attach to in one
    file's token stream. Read from tokens rather than text so that a reconstruction cannot merge or
    split the attribute (`lex` separates every punctuation character)."""
    names, i = set(), 0
    while i < len(tokens):
        kind, text = tokens[i]
        if kind != 'code' or text != '#' or i + 1 >= len(tokens) or tokens[i + 1][1] != '[':
            i += 1
            continue
        close, attr = i + 2, []
        while close < len(tokens) and not (tokens[close][0] == 'code' and tokens[close][1] == ']'):
            attr.append(tokens[close])
            close += 1
        words = [value for _, value in attr]
        if 'export_name' in words:
            names.update(value for token_kind, value in attr if token_kind == 'string')
        if 'no_mangle' in words:
            k = close + 1
            while k < len(tokens):
                token_kind, value = tokens[k]
                if token_kind == 'code' and value == 'fn':
                    if k + 1 < len(tokens):
                        names.add(tokens[k + 1][1])
                    break
                if token_kind == 'code' and value in (';', '{'):
                    break
                k += 1
        i = close + 1
    return names


def rust_exports(root):
    """Every name the Rust tree exports from NON-TEST code.

    Test items are dropped the way the other Rust gates drop them (`#[cfg(test)]` modules and
    functions): an export that only test code declares does not exist in the ARM build, so it cannot
    satisfy `src/main.c`. An export gated on `feature = "test-support"` is still counted — the
    shipping build disables that feature, so the ARM link stays the authority for that one, and no
    such export exists today."""
    names = set()
    for path in sorted((root / RUST_DIR).rglob('*.rs')):
        tokens = without_test_items(rust_test_modules.lex(path.read_text(errors='replace')))
        names |= exports_in(tokens)
    return names


def problems(root=REPO):
    root = Path(root)
    c_files = sorted((root / C_DIR).glob('*.c')) + sorted((root / C_DIR).glob('*.h'))
    if not c_files:
        return [f'no C sources under {C_DIR}/']
    defined = c_definitions(c_files) | rust_exports(root)
    found = []
    for path in c_files:
        for name, line in declarations(path):
            if name not in defined:
                rel = path.relative_to(root)
                found.append(f'{rel}:{line}: extern {name}() is defined by neither src/*.c nor a '
                             f'Rust #[no_mangle]/#[export_name] export — the ARM link would fail')
    return found


def main():
    found = problems()
    for line in found:
        print(line)
    if found:
        print(f'ffi-symbols: FAILED ({len(found)} unresolved)')
        return 1
    print('ffi-symbols: ok (every extern in src/*.c resolves to C or a Rust export)')
    return 0


if __name__ == '__main__':
    sys.exit(main())
