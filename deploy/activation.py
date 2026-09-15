#!/usr/bin/env python3
"""Reconcile the durable release/config activation guard before startup."""

import argparse
import json
import os
import shutil
import stat
import sys
import tempfile
from pathlib import Path
from typing import Optional


def sync_directory(path: Path) -> None:
    fd = os.open(path, os.O_DIRECTORY)
    try:
        os.fsync(fd)
    finally:
        os.close(fd)


def atomic_json(path: Path, value: dict) -> None:
    path.parent.mkdir(mode=0o700, parents=True, exist_ok=True)
    fd, temporary = tempfile.mkstemp(prefix=f".{path.name}.", dir=path.parent)
    try:
        with os.fdopen(fd, "w", encoding="utf-8") as stream:
            json.dump(value, stream, sort_keys=True)
            stream.write("\n")
            stream.flush()
            os.fsync(stream.fileno())
        os.chmod(temporary, 0o600)
        os.replace(temporary, path)
        sync_directory(path.parent)
    finally:
        try:
            os.unlink(temporary)
        except FileNotFoundError:
            pass


def load_record(path: Path) -> dict:
    try:
        with path.open(encoding="utf-8") as stream:
            record = json.load(stream)
    except (OSError, json.JSONDecodeError) as error:
        raise RuntimeError(f"cannot read activation record: {error}") from error
    if record.get("version") != 1:
        raise RuntimeError("unsupported activation record version")
    required = {"current_link", "config_path", "intended_release", "intended_config"}
    missing = required - record.keys()
    if missing:
        raise RuntimeError(f"activation record is missing: {', '.join(sorted(missing))}")
    return record


def absolute_path(record: dict, name: str, required: bool = True) -> Optional[Path]:
    raw = record.get(name, "")
    if not raw:
        if required:
            raise RuntimeError(f"activation record has no {name}")
        return None
    path = Path(raw)
    if not path.is_absolute():
        raise RuntimeError(f"activation record {name} is not absolute")
    return path


def release_is_usable(path: Path) -> bool:
    return path.is_dir() and (path / "buzz-server").is_file() and os.access(path / "buzz-server", os.X_OK)


def install_config(source: Path, destination: Path) -> None:
    if not source.is_file():
        raise RuntimeError(f"activation config is missing: {source}")
    destination.parent.mkdir(mode=0o750, parents=True, exist_ok=True)
    fd, temporary = tempfile.mkstemp(prefix=f".{destination.name}.", dir=destination.parent)
    try:
        with os.fdopen(fd, "wb") as output, source.open("rb") as input_stream:
            shutil.copyfileobj(input_stream, output)
            output.flush()
            os.fchmod(output.fileno(), stat.S_IRUSR | stat.S_IWUSR | stat.S_IRGRP)
            os.fsync(output.fileno())
        os.replace(temporary, destination)
        sync_directory(destination.parent)
    finally:
        try:
            os.unlink(temporary)
        except FileNotFoundError:
            pass


def set_current(link: Path, release: Path) -> None:
    link.parent.mkdir(mode=0o755, parents=True, exist_ok=True)
    fd, temporary = tempfile.mkstemp(prefix=f".{link.name}.", dir=link.parent)
    os.close(fd)
    os.unlink(temporary)
    os.symlink(release, temporary)
    os.replace(temporary, link)
    sync_directory(link.parent)


def clear_record(path: Path) -> None:
    try:
        path.unlink()
    except FileNotFoundError:
        return
    sync_directory(path.parent)


def reconcile(path: Path) -> None:
    if not path.exists():
        return
    record = load_record(path)
    current_link = absolute_path(record, "current_link")
    config_path = absolute_path(record, "config_path")
    intended_release = absolute_path(record, "intended_release")
    intended_config = absolute_path(record, "intended_config")
    previous_release = absolute_path(record, "previous_release", required=False)
    previous_config = absolute_path(record, "previous_config", required=False)
    if not release_is_usable(intended_release):
        raise RuntimeError(f"intended release is unusable: {intended_release}")
    if previous_release is not None and not release_is_usable(previous_release):
        raise RuntimeError(f"previous release is unusable: {previous_release}")
    if previous_config is not None and not previous_config.is_file():
        raise RuntimeError(f"previous config is missing: {previous_config}")

    current_target = Path(os.path.realpath(current_link)) if current_link.exists() else None
    if current_target is None:
        selected_release = intended_release
        selected_config = intended_config
    elif current_target == intended_release:
        selected_release = intended_release
        selected_config = intended_config
    elif previous_release is not None and current_target == previous_release:
        selected_release = previous_release
        selected_config = previous_config
    else:
        raise RuntimeError(f"current release is outside activation record: {current_target}")

    if selected_config is None:
        raise RuntimeError("activation record cannot restore a previous config")
    # Keep the existing release selected while replacing its config. If the
    # pointer is absent, write the intended config first, then publish the
    # intended pointer so a start can never observe a mismatched pair.
    install_config(selected_config, config_path)
    if current_target is None:
        set_current(current_link, selected_release)
    clear_record(path)


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("action", choices=("prepare", "reconcile", "clear"))
    parser.add_argument("--record", required=True, type=Path)
    parser.add_argument("--current-link", type=Path)
    parser.add_argument("--config-path", type=Path)
    parser.add_argument("--previous-release", type=Path)
    parser.add_argument("--previous-config", type=Path)
    parser.add_argument("--intended-release", type=Path)
    parser.add_argument("--intended-config", type=Path)
    args = parser.parse_args()
    try:
        if args.action == "prepare":
            values = {
                "version": 1,
                "current_link": str(args.current_link),
                "config_path": str(args.config_path),
                "previous_release": str(args.previous_release) if args.previous_release else "",
                "previous_config": str(args.previous_config) if args.previous_config else "",
                "intended_release": str(args.intended_release),
                "intended_config": str(args.intended_config),
            }
            atomic_json(args.record, values)
        elif args.action == "reconcile":
            reconcile(args.record)
        else:
            clear_record(args.record)
    except (OSError, RuntimeError) as error:
        print(f"activation recovery failed: {error}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
