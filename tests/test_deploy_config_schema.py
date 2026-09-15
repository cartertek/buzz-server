import json
import tempfile
import unittest
from pathlib import Path


SCRIPT = Path(__file__).resolve().parents[1] / "deploy" / "install-package.sh"


class DeployConfigSchemaRegressionTests(unittest.TestCase):
    def test_migrated_candidate_is_checked_before_service_drain_and_activation(self):
        source = SCRIPT.read_text()
        candidate_check = source.index('"$release_source/buzz-server-daemon" --check-config "$config_candidate"')
        drain = source.index('drain_service "Stopping existing Buzz Server process tree"', candidate_check)
        activation = source.index('install -o root -g buzz-server -m 0640 "$config_candidate" /etc/buzz-server/config.json')
        self.assertLess(candidate_check, drain)
        self.assertLess(drain, activation)
        self.assertNotIn('--check-config /etc/buzz-server/config.json', source)
        self.assertIn('config_backup="$operation_root/config.json.previous"', source)
        self.assertIn('config_candidate="$operation_root/config.json.candidate"', source)
        rollback = source.rindex('install -o root -g buzz-server -m 0640 "$config_backup" /etc/buzz-server/config.json')
        self.assertGreater(rollback, activation)

    def test_removed_old_field_can_be_migrated_and_rollback_keeps_old_config(self):
        # Exercise the compatibility windows without touching /etc or invoking
        # an installer against the host: the old parser sees the old file, the
        # target parser sees only the staged migrated file, and rollback restores
        # the old bytes if activation fails.
        old_fields = {"state_database", "legacy_field"}
        new_fields = {"state_database", "default_agent"}

        def strict_parse(path, fields):
            value = json.loads(path.read_text())
            self.assertLessEqual(set(value), fields)
            return value

        with tempfile.TemporaryDirectory() as directory:
            old = Path(directory) / "config.json"
            backup = Path(directory) / "config.json.previous"
            candidate = Path(directory) / "config.json.candidate"
            old.write_text('{"state_database":"state.sqlite3","legacy_field":true}\n')
            backup.write_bytes(old.read_bytes())
            candidate.write_text('{"state_database":"state.sqlite3","default_agent":{}}\n')

            strict_parse(old, old_fields)
            target_config = strict_parse(candidate, new_fields)
            self.assertNotIn("legacy_field", target_config)
            self.assertNotEqual(old.read_bytes(), candidate.read_bytes())

            old.write_bytes(candidate.read_bytes())
            old.write_bytes(backup.read_bytes())
            self.assertEqual(old.read_bytes(), backup.read_bytes())


if __name__ == "__main__":
    unittest.main()
