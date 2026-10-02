#!/usr/bin/env python3
"""Emit action-level native copy evidence from existing focused tests."""

import json
import os
import platform
import re
import subprocess
import sys
from pathlib import Path

output = Path("copy-capability-matrix.json")
platform_name = platform.system().lower()
base_command = ["cargo", "test", "--locked"]
records = []
failed = False
running_tests_pattern = re.compile(r"running (\d+) tests?")
passed_tests_pattern = re.compile(r"test result: (?:ok|FAILED)\. (\d+) passed;")


def run(action, phase, test_filter, capability, target_args):
    global failed
    command = [*base_command, *target_args, test_filter]
    result = subprocess.run(command, text=True, capture_output=True, env=os.environ.copy())
    output_text = result.stdout + result.stderr
    matched_tests = sum(int(count) for count in running_tests_pattern.findall(output_text))
    passed_tests = sum(int(count) for count in passed_tests_pattern.findall(output_text))
    log = Path(f"copy-capability-{action}-{phase}.log")
    log.write_text(output_text, encoding="utf-8")
    outcome = "passed" if result.returncode == 0 and matched_tests == 1 and passed_tests == 1 else "failed"
    failure = None
    if result.returncode != 0:
        failure = f"cargo exited with status {result.returncode}"
    elif matched_tests != 1:
        failure = f"test filter matched {matched_tests} tests; expected exactly one"
    elif passed_tests != 1:
        failure = f"test filter produced {passed_tests} passing tests; expected exactly one"
    records.append(
        {
            "action": action,
            "capability": capability,
            "phase": phase,
            "command": command,
            "outcome": outcome,
            "matched_tests": matched_tests,
            "passed_tests": passed_tests,
            "log": str(log),
            **({"failure": failure} if failure else {}),
        }
    )
    failed = failed or outcome == "failed"


def record_unrun(action, phase, reason):
    records.append(
        {
            "action": action,
            "capability": "fail_closed",
            "phase": phase,
            "outcome": "unrun",
            "reason": reason,
        }
    )


if platform_name in ("linux", "darwin"):
    for action, test_filter in [
        ("create_copy", "create_primitive_uses_only_the_recorded_temporary_and_verifies_its_postcondition"),
        ("replace_copy", "replace_requires_the_recorded_old_copy_and_verifies_the_new_copy"),
        ("remove_copy", "remove_requires_the_exact_owned_fingerprint"),
        ("relocate_copy", "relocate_publishes_the_new_copy_before_removing_the_old_copy"),
    ]:
        capability = "fail_closed"
        run(action, "executor_postcondition", test_filter, capability, ["--lib"])
    for action, test_filter in [
        ("link_to_copy_handoff", "link_to_copy_handoff_replaces_only_the_expected_managed_link"),
        ("copy_to_link_handoff", "copy_to_link_handoff_replaces_only_the_expected_managed_copy"),
    ]:
        run(action, "executor_postcondition", test_filter, "fail_closed", ["--lib"])
        run(action, "preflight_rejection", "copy_handoffs_fail_preflight_without_mutating_target_or_state", "fail_closed", ["--test", "cli_read_only"])
    for action, test_filter in [
        ("create_copy", "copy_lifecycle_commands_render_typed_actions_and_fail_closed_before_mutation"),
        ("replace_copy", "copy_replace_relocate_and_remove_fail_preflight_without_mutating_target_or_state"),
        ("remove_copy", "copy_replace_relocate_and_remove_fail_preflight_without_mutating_target_or_state"),
        ("relocate_copy", "copy_replace_relocate_and_remove_fail_preflight_without_mutating_target_or_state"),
    ]:
        run(action, "compiled_binary_fail_closed", test_filter, "fail_closed", ["--test", "cli_read_only"])
    for action, test_filter in [
        ("create_copy", "recovery_keeps_matching_final_copy_create_uncertain_without_known_update"),
        ("replace_copy", "recovery_cleans_an_exact_copy_temporary_then_fails_and_keeps_old_known"),
        ("remove_copy", "recovery_removes_copy_known_only_for_the_recorded_missing_postcondition"),
        ("relocate_copy", "recovery_keeps_a_partial_copy_relocation_uncertain_with_old_known"),
        ("link_to_copy_handoff", "recovery_classifies_link_to_copy_handoff_aftermath_from_recorded_effects"),
        ("copy_to_link_handoff", "recovery_classifies_copy_to_link_handoff_aftermath_from_recorded_effects"),
    ]:
        run(action, "post_effect_aftermath", test_filter, "fail_closed", ["--lib"])
        run(action, "recovery", test_filter, "fail_closed", ["--lib"])
else:
    run("create_copy", "preflight_rejection", "copy_capability_preflight_rejection_creates_no_operation_or_target", "fail_closed", ["--test", "cli_read_only"])
    for action in ["replace_copy", "remove_copy", "relocate_copy"]:
        run(
            action,
            "preflight_rejection",
            "copy_replace_relocate_and_remove_fail_preflight_without_mutating_target_or_state",
            "fail_closed",
            ["--test", "cli_read_only"],
        )
    record_unrun(
        "link_to_copy_handoff",
        "native_preflight_rejection",
        "requires a Windows file-symbolic-link fixture; no native symlink-policy evidence was collected",
    )
    record_unrun(
        "copy_to_link_handoff",
        "native_preflight_rejection",
        "requires a Windows file-symbolic-link publication fixture; no native symlink-policy evidence was collected",
    )

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
