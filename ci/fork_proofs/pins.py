"""Fork Rofocale11: fork-owned certificate pins for the two readonly proofs.

Upstream's proof scripts and their `ci/proofs/*.json` certificates stay
byte-for-byte upstream so an upstream merge never conflicts on them. They
cannot certify this tree as-is: the certificates enumerate the workspace's
crate manifests and lockfile, and Nuvio Native adds crates upstream has no
concept of. Keep the fork's reviewed certificates here instead; the two fork
test files route the upstream modules to these pins before running.

Regenerate both certificates in this directory whenever the workspace member
set or Cargo.lock changes (an upstream merge always does both):

    python3 ci/readonly_dependencies.py --produce <verified serde_json 1.0.154 crate> \
        --root rust-modules --output ci/fork_proofs/serde-json-immutable.json
    python3 - <<'PY'  # from ci/
    import json
    from pathlib import Path
    import readonly_generated as g
    cert = g.produce_certificate(Path('../rust-modules').resolve())
    Path('fork_proofs/platform-generated-enums.json').write_text(
        json.dumps(cert, indent=2, sort_keys=True) + '\n', encoding='utf-8')
    PY

Then update the two SHA constants below with the producer's printed pins.
"""
from pathlib import Path

HERE = Path(__file__).resolve().parent
GENERATED_CERT = HERE / 'platform-generated-enums.json'
DEPS_CERT = HERE / 'serde-json-immutable.json'
GENERATED_SHA256 = '5e0a079274db6c708b785479171bb762a5445c55669b46d70377c9ea2bb87424'
DEPS_SHA256 = '99359cba57607f06069968cfa69e3326e4a26c9554ea800bc55603eb6e7b2844'


def apply(dependencies, generated=None):
    """Route upstream's proof modules to the fork's reviewed certificates."""
    dependencies.CERTIFICATE_PATH = DEPS_CERT
    dependencies.CERTIFICATE_SHA256 = DEPS_SHA256
    if generated is not None:
        generated.CERTIFICATE_PATH = GENERATED_CERT
        generated.CERTIFICATE_SHA256 = GENERATED_SHA256
