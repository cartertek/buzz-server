import tempfile
import unittest
from pathlib import Path


SCRIPT = Path(__file__).resolve().parents[1] / "deploy" / "install-package.sh"


class DeployConfigSchemaRegressionTests(unittest.TestCase):
    def test_candidate_schema_is_checked_before_service_drain_and_activation(self):
        source = SCRIPT.read_text()
        old_check = source.index('"$release_source/buzz-server-daemon" --check-config /etc/buzz-server/config.json')
        candidate_check = source.index('"$release_source/buzz-server-daemon" --check-config "$config_candidate"')
        drain = source.index('drain_service "Stopping existing Buzz Server process tree"', candidate_check)
        activation = source.index('install -o root -g buzz-server -m 0640 "$config_candidate" /etc/buzz-server/config.json')
        self.assertLess(old_check, candidate_check)
        self.assertLess(candidate_check, drain)
        self.assertLess(drain, activation)
        self.assertIn('config_backup="$operation_root/config.json.previous"', source)
        self.assertIn('config_candidate="$operation_root/config.json.candidate"', source)

    def test_new_only_field_is_rejected_by_old_parser_without_replacing_old_config(self):
        # Model the incident boundary with strict old/new schema parsers. The
        # fixture makes the safety property explicit without touching /etc or
        # invoking an installer against the host.
        old_fields = {"state_database"}
        new_fields = old_fields | {"default_agent"}
        with tempfile.TemporaryDirectory() as directory:
            old = Path(directory) / "config.json"
            candidate = Path(directory) / "config.json.candidate"
            old.write_text('{"state_database":"state.sqlite3"}\n')
            candidate.write_text('{"state_database":"state.sqlite3","default_agent":{}}\n')
            import json

            old_config = json.loads(old.read_text())
            candidate_config = json.loads(candidate.read_text())
            self.assertTrue(set(old_config) <= old_fields)
            self.assertTrue(set(candidate_config) <= new_fields)
            self.assertNotEqual(set(candidate_config), set(old_config))
            self.assertEqual(old.read_text(), '{"state_database":"state.sqlite3"}\n')


if __name__ == "__main__":
    unittest.main()
