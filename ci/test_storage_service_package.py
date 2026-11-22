"""Fork Rofocale11: Nuvio Native packages NO storage helper — one redb file inside the app.

The upstream test graded the private DB8 service archive. The fork dissolved that service into
`plx_os::storage::redb_backend`, so the grade is now negative: a service payload, a service
declaration or any persisted credential/rendezvous file in the ipk is a regression.
"""
import io
import json
import tarfile
import tempfile
import unittest
from pathlib import Path

import mkipk


class StorageServicePackage(unittest.TestCase):
    def stage_app(self, data, app):
        appdir = data / "usr/palm/applications" / app["id"]
        appdir.mkdir(parents=True)
        (appdir / "appinfo.json").write_text(json.dumps({**app, "requiredPermissions": mkipk.STORAGE_PERMISSIONS}))

    def test_every_flavor_has_no_storage_service_and_no_writable_state(self):
        for flav in mkipk.flavor.FLAVORS:
            app_id = mkipk.flavor.app_id(flav)
            with self.subTest(app_id=app_id), tempfile.TemporaryDirectory() as tmp:
                root = Path(tmp)
                app = mkipk.flavor.appinfo_for(flav, "20260919")
                data = root / "data"
                self.stage_app(data, app)
                mkipk.write_packageinfo(data, app)
                archive = root / "data.tar.gz"
                mkipk.write_targz(archive, data, "")
                self.assertEqual(mkipk.storage_archive_errors(archive.read_bytes(), app_id), [])
                first = archive.read_bytes()
                mkipk.write_targz(archive, data, "")
                self.assertEqual(first, archive.read_bytes())
                with tarfile.open(fileobj=io.BytesIO(archive.read_bytes()), mode="r:gz") as tf:
                    package = json.load(tf.extractfile(f"usr/palm/packages/{app_id}/packageinfo.json"))
                    self.assertEqual(package["services"], [])
                    self.assertEqual(package["requiredPermissions"], mkipk.STORAGE_PERMISSIONS)
                    application = json.load(tf.extractfile(f"usr/palm/applications/{app_id}/appinfo.json"))
                    self.assertEqual(application["requiredPermissions"], package["requiredPermissions"])
                    self.assertFalse(any("/state" in m.name for m in tf.getmembers()))
                    self.assertFalse(any(m.name.endswith("/plxnative-storage") for m in tf.getmembers()))

    def test_archive_checker_rejects_a_packaged_helper_service(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            foreign = root / "usr/palm/services/com.nuvionative.app.debug.storage"
            foreign.mkdir(parents=True)
            (foreign / "plxnative-storage").write_bytes(b"\x7fELF")
            archive = root.parent / (root.name + ".tar.gz")
            try:
                mkipk.write_targz(archive, root, "")
                errors = mkipk.storage_archive_errors(archive.read_bytes(), "com.nuvionative.app")
                self.assertIn("Nuvio Native must not package a storage helper service", errors)
            finally:
                archive.unlink(missing_ok=True)

    def test_archive_checker_rejects_obsolete_state(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            (root / "usr/palm/applications/com.nuvionative.app/state").mkdir(parents=True)
            archive = root.parent / (root.name + ".tar.gz")
            try:
                mkipk.write_targz(archive, root, "")
                errors = mkipk.storage_archive_errors(archive.read_bytes(), "com.nuvionative.app")
                self.assertIn("obsolete writable app state is forbidden", errors)
            finally:
                archive.unlink(missing_ok=True)

    def test_archive_checker_rejects_private_session_records(self):
        for name in ("session.json", "consent.json"):
            with self.subTest(name=name), tempfile.TemporaryDirectory() as tmp:
                root = Path(tmp)
                data = root / "data/usr/palm/applications/com.nuvionative.app"
                data.mkdir(parents=True)
                (data / name).write_text("{}")
                archive = root / "data.tar.gz"
                mkipk.write_targz(archive, root / "data", "")
                errors = mkipk.storage_archive_errors(archive.read_bytes(), "com.nuvionative.app")
                self.assertIn("runtime credentials or rendezvous files must not be packaged", errors)


if __name__ == "__main__":
    unittest.main()
