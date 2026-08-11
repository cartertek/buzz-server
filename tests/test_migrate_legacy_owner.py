import importlib.util
import json
import sqlite3
import tempfile
import unittest
from pathlib import Path

MODULE_PATH = Path(__file__).resolve().parents[1] / "deploy" / "migrate-legacy-owner.py"
spec = importlib.util.spec_from_file_location("migrate_legacy_owner", MODULE_PATH)
module = importlib.util.module_from_spec(spec)
spec.loader.exec_module(module)


class LegacyOwnerMigrationTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.root = Path(self.temp.name)
        self.database = self.root / "state.sqlite3"
        connection = sqlite3.connect(self.database)
        connection.execute("CREATE TABLE community_configs(id TEXT PRIMARY KEY, document TEXT NOT NULL, updated_at INTEGER NOT NULL)")
        connection.commit()
        connection.close()
        self.config = self.root / "config.json"

    def tearDown(self):
        self.temp.cleanup()

    def write_config(self, **extra):
        config = {
            "state_database": str(self.database),
            "owner_secret_file": "/run/buzz-server/credentials/owner-secret",
            **extra,
        }
        self.config.write_text(json.dumps(config))

    def put_community(self, community_id, identity_pubkey=None):
        document = {
            "id": community_id,
            "display_name": community_id,
            "relay_url": "wss://relay.example/",
        }
        if identity_pubkey:
            document["identity_pubkey"] = identity_pubkey
        connection = sqlite3.connect(self.database)
        connection.execute(
            "INSERT INTO community_configs(id, document, updated_at) VALUES(?,?,0)",
            (community_id, json.dumps(document)),
        )
        connection.commit()
        connection.close()

    def read_community(self, community_id):
        connection = sqlite3.connect(self.database)
        document = connection.execute(
            "SELECT document FROM community_configs WHERE id=?", (community_id,)
        ).fetchone()[0]
        connection.close()
        return json.loads(document)

    def test_migrates_missing_identity_and_removes_global_config(self):
        owner = "a" * 64
        self.write_config()
        self.put_community("legacy")
        self.assertTrue(module.migrate(self.config, owner))
        self.assertNotIn("owner_secret_file", json.loads(self.config.read_text()))
        self.assertEqual(self.read_community("legacy")["identity_pubkey"], owner)

    def test_preserves_already_associated_community_identity(self):
        owner = "a" * 64
        existing = "b" * 64
        self.write_config()
        self.put_community("current", existing)
        module.migrate(self.config, owner)
        self.assertEqual(self.read_community("current")["identity_pubkey"], existing)


    def test_preserves_kms_custody_configuration(self):
        owner = "a" * 64
        self.write_config()
        self.put_community("legacy")
        module.migrate(self.config, owner, "alias/buzz-owner")
        config = json.loads(self.config.read_text())
        self.assertEqual(config["identity_custody"]["kms_key_id"], "alias/buzz-owner")
        self.assertEqual(self.read_community("legacy")["identity_pubkey"], owner)

    def test_current_config_is_noop(self):
        self.config.write_text(json.dumps({"state_database": str(self.database)}))
        self.assertFalse(module.migrate(self.config, "a" * 64))


if __name__ == "__main__":
    unittest.main()
