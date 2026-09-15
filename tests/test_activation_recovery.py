import subprocess
import sys
import tempfile
import unittest
from pathlib import Path
from unittest import mock


HELPER = Path(__file__).resolve().parents[1] / "deploy" / "activation.py"
UNIT = Path(__file__).resolve().parents[1] / "deploy" / "buzz-server.service"
sys.path.insert(0, str(HELPER.parent))
import activation


class ActivationRecoveryTests(unittest.TestCase):
    def setUp(self):
        self.directory = tempfile.TemporaryDirectory()
        self.root = Path(self.directory.name)
        self.current = self.root / "current"
        self.config = self.root / "config.json"
        self.previous = self.root / "releases" / "previous"
        self.intended = self.root / "releases" / "intended"
        self.previous.mkdir(parents=True)
        self.intended.mkdir(parents=True)
        for release in (self.previous, self.intended):
            daemon = release / "buzz-server"
            daemon.write_text("#!/bin/sh\n")
            daemon.chmod(0o755)
        self.old_config = b'{"schema":"old"}\n'
        self.new_config = b'{"schema":"new"}\n'
        self.config.write_bytes(self.old_config)
        self.record = self.root / "activation.json"
        self.previous_config = self.root / "previous.json"
        self.previous_config.write_bytes(self.old_config)
        self.intended_config = self.root / "candidate.json"
        self.intended_config.write_bytes(self.new_config)

    def tearDown(self):
        self.directory.cleanup()

    def prepare(self, previous=True):
        command = [
            "python3",
            str(HELPER),
            "prepare",
            "--record",
            str(self.record),
            "--current-link",
            str(self.current),
            "--config-path",
            str(self.config),
            "--previous-config",
            str(self.previous_config),
            "--intended-release",
            str(self.intended),
            "--intended-config",
            str(self.intended_config),
        ]
        if previous:
            command.extend(("--previous-release", str(self.previous)))
        subprocess.run(command, check=True)

    def reconcile(self):
        subprocess.run(
            ["python3", str(HELPER), "reconcile", "--record", str(self.record)],
            check=True,
        )

    def reconcile_with_fchown_spy(self, source):
        with mock.patch.object(activation.os, "fchown", wraps=activation.os.fchown) as fchown:
            activation.reconcile(self.record)
        self.assertEqual(fchown.call_count, 1)
        _, uid, gid = fchown.call_args.args
        source_metadata = source.stat()
        self.assertEqual(uid, source_metadata.st_uid)
        self.assertEqual(gid, source_metadata.st_gid)

    def assert_config_metadata(self, source):
        metadata = self.config.stat()
        source_metadata = source.stat()
        self.assertEqual(metadata.st_uid, source_metadata.st_uid)
        self.assertEqual(metadata.st_gid, source_metadata.st_gid)
        self.assertEqual(metadata.st_mode & 0o777, 0o640)

    def test_before_first_swap_keeps_previous_pair_and_is_idempotent(self):
        self.current.symlink_to(self.previous)
        self.prepare()
        self.reconcile_with_fchown_spy(self.previous_config)
        self.assertEqual(self.current.resolve(), self.previous)
        self.assertEqual(self.config.read_bytes(), self.old_config)
        self.assert_config_metadata(self.previous_config)
        self.reconcile()
        self.assertFalse(self.record.exists())

    def test_between_swaps_completes_intended_pair(self):
        self.current.symlink_to(self.intended)
        self.prepare()
        self.reconcile_with_fchown_spy(self.intended_config)
        self.assertEqual(self.current.resolve(), self.intended)
        self.assertEqual(self.config.read_bytes(), self.new_config)
        self.assert_config_metadata(self.intended_config)
        self.assertFalse(self.record.exists())

    def test_after_second_swap_only_clears_guard(self):
        self.current.symlink_to(self.intended)
        self.config.write_bytes(self.new_config)
        self.prepare()
        self.reconcile()
        self.assertEqual(self.config.read_bytes(), self.new_config)
        self.assert_config_metadata(self.intended_config)
        self.assertFalse(self.record.exists())

    def test_missing_pointer_publishes_intended_pair_after_config(self):
        self.prepare(previous=False)
        self.reconcile()
        self.assertEqual(self.current.resolve(), self.intended)
        self.assertEqual(self.config.read_bytes(), self.new_config)
        self.assert_config_metadata(self.intended_config)
        self.assertFalse(self.record.exists())

    def test_unknown_pointer_fails_closed_and_keeps_guard(self):
        unknown = self.root / "releases" / "unknown"
        unknown.mkdir()
        (unknown / "buzz-server").write_text("#!/bin/sh\n")
        (unknown / "buzz-server").chmod(0o755)
        self.current.symlink_to(unknown)
        self.prepare()
        result = subprocess.run(
            ["python3", str(HELPER), "reconcile", "--record", str(self.record)],
            capture_output=True,
            text=True,
        )
        self.assertNotEqual(result.returncode, 0)
        self.assertTrue(self.record.exists())
        self.assertEqual(self.config.read_bytes(), self.old_config)

    def test_service_reconciles_before_daemon_start(self):
        source = UNIT.read_text()
        reconcile = source.index("activation.py reconcile")
        daemon = source.index("ExecStart=/opt/buzz-server/current/buzz-server")
        self.assertLess(reconcile, daemon)


if __name__ == "__main__":
    unittest.main()
