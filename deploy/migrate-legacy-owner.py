#!/usr/bin/env python3
"""Migrate the former global owner configuration to per-community identity references."""

import argparse
import json
import os
import sqlite3
import tempfile
from pathlib import Path
from typing import Optional


def migrate(config_path: Path, owner_pubkey: str, kms_key_id: Optional[str] = None) -> bool:
    config = json.loads(config_path.read_text())
    if "owner_secret_file" not in config:
        return False

    owner_pubkey = owner_pubkey.strip().lower()
    if len(owner_pubkey) != 64 or any(c not in "0123456789abcdef" for c in owner_pubkey):
        raise ValueError("legacy owner public key must be 64 lowercase hex characters")

    database = Path(config.get("state_database", "/var/lib/buzz-server/state.sqlite3"))
    if not database.exists():
        raise RuntimeError("legacy owner configuration exists but state database is missing")

    connection = sqlite3.connect(database)
    try:
        connection.execute("BEGIN IMMEDIATE")
        rows = connection.execute("SELECT id, document FROM community_configs").fetchall()
        for community_id, document in rows:
            community = json.loads(document)
            if not community.get("identity_pubkey"):
                community["identity_pubkey"] = owner_pubkey
                connection.execute(
                    "UPDATE community_configs SET document=? WHERE id=?",
                    (json.dumps(community, separators=(",", ":")), community_id),
                )
        connection.commit()
    except Exception:
        connection.rollback()
        raise
    finally:
        connection.close()

    del config["owner_secret_file"]
    if kms_key_id:
        config.setdefault("identity_custody", {})["kms_key_id"] = kms_key_id
    fd, temporary_name = tempfile.mkstemp(prefix=config_path.name + ".", dir=config_path.parent)
    try:
        with os.fdopen(fd, "w") as temporary:
            json.dump(config, temporary, indent=2)
            temporary.write("\n")
            temporary.flush()
            os.fsync(temporary.fileno())
        os.chmod(temporary_name, 0o640)
        os.replace(temporary_name, config_path)
    except Exception:
        try:
            os.unlink(temporary_name)
        except FileNotFoundError:
            pass
        raise
    return True


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("config", type=Path)
    parser.add_argument("owner_pubkey")
    parser.add_argument("--kms-key-id")
    args = parser.parse_args()
    return 0 if migrate(args.config, args.owner_pubkey, args.kms_key_id) else 3


if __name__ == "__main__":
    raise SystemExit(main())
