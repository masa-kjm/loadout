#!/usr/bin/env python3
"""Emit action-level native copy evidence from existing focused tests."""

import json
import os
import platform
import subprocess
import sys
from pathlib import Path

output = Path("copy-capability-matrix.json")
platform_name = platform.system().lower()
base_command = ["cargo", "test", "--locked"]
records = []
failed = False


def run(action, phase, test_filter, capability):
    global failed
    command = [*base_command, test_filter]
    result = subprocess.run(command, text=True, capture_output=True, env=os.environ.copy())
    log = Path(f"copy-capability-{action}-{phase}.log")
    log.write_text(result.stdout + result.stderr, encoding="utf-8")
    outcome = "passed" if result.returncode == 0 else "failed"
    records.append(
        {
            "action": action,
            "capability": capability,
            "phase": phase,
            "command": command,
            "outcome": outcome,
            "log": str(log),
        }
    )
    failed = failed or result.returncode != 0


if platform_name in ("linux", "darwin"):
    for action, test_filter in [
        ("create_copy", "create_uses_only_the_recorded_temporary_and_verifies_its_postcondition"),
        ("replace_copy", "replace_requires_the_recorded_old_copy_and_verifies_the_new_copy"),
        ("remove_copy", "remove_requires_the_exact_owned_fingerprint"),
        ("relocate_copy", "relocate_publishes_the_new_copy_before_removing_the_old_copy"),
    ]:
        capability = "fail_closed" if action.endswith("handoff") else "enabled"
        run(action, "executor_postcondition", test_filter, capability)
    for action in ["link_to_copy_handoff", "copy_to_link_handoff"]:
        run(action, "preflight_rejection", "copy_handoffs_fail_preflight_without_mutating_target_or_state", "fail_closed")
    for action in ["create_copy", "replace_copy", "remove_copy", "relocate_copy"]:
        run(action, "compiled_binary", "copy_lifecycle_commands_render_typed_actions_and_preserve_rejection_boundaries", "enabled")
    for action, test_filter in [
        ("create_copy", "recovery_keeps_matching_final_copy_create_uncertain_without_known_update"),
        ("replace_copy", "recovery_cleans_an_exact_copy_temporary_then_fails_and_keeps_old_known"),
        ("remove_copy", "recovery_removes_copy_known_only_for_the_recorded_missing_postcondition"),
        ("relocate_copy", "recovery_keeps_a_partial_copy_relocation_uncertain_with_old_known"),
    ]:
        run(action, "post_effect_aftermath", test_filter, "enabled")
        run(action, "recovery", test_filter, "enabled")
else:
    for action in [
        "create_copy",
        "replace_copy",
        "remove_copy",
        "relocate_copy",
        "link_to_copy_handoff",
        "copy_to_link_handoff",
    ]:
        run(action, "preflight_rejection", "copy_capability_preflight_rejection_creates_no_operation_or_target", "fail_closed")

output.write_text(
    json.dumps(
        {
            "schema_version": 1,
            "platform": platform_name,
            "fixture_root": os.environ.get("LOADOUT_NATIVE_FIXTURE_ROOT"),
            "temporary_environment": {
                key: os.environ.get(key) for key in ("TMPDIR", "TEMP", "TMP")
            },
            "records": records,
        },
        indent=2,
    )
    + "\n",
    encoding="utf-8",
)

sys.exit(1 if failed else 0)
