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


def run_sequential_copy_candidates():
    """Record candidate-only executor and recovery evidence for each sequential action."""
    candidates = [
        (
            "replace_copy",
            "replace_requires_the_recorded_old_copy_and_verifies_the_new_copy",
            "recovery_commits_or_fails_copy_replacement_from_recorded_conditions",
            "copy_replace_relocate_and_remove_fail_preflight_without_mutating_target_or_state",
        ),
        (
            "remove_copy",
            "remove_requires_the_exact_owned_fingerprint",
            "recovery_removes_copy_known_only_for_the_recorded_missing_postcondition",
            "copy_replace_relocate_and_remove_fail_preflight_without_mutating_target_or_state",
        ),
        (
            "relocate_copy",
            "relocate_publishes_the_new_copy_before_removing_the_old_copy",
            "recovery_commits_or_fails_copy_relocation_from_recorded_conditions",
            "copy_replace_relocate_and_remove_fail_preflight_without_mutating_target_or_state",
        ),
        (
            "link_to_copy_handoff",
            "link_to_copy_handoff_replaces_only_the_expected_managed_link",
            "recovery_classifies_link_to_copy_handoff_aftermath_from_recorded_effects",
            "copy_handoffs_fail_preflight_without_mutating_target_or_state",
        ),
        (
            "copy_to_link_handoff",
            "copy_to_link_handoff_replaces_only_the_expected_managed_copy",
            "recovery_classifies_copy_to_link_handoff_aftermath_from_recorded_effects",
            "copy_handoffs_fail_preflight_without_mutating_target_or_state",
        ),
    ]
    for action, executor_test, recovery_test, preflight_test in candidates:
        run(action, "executor_candidate", executor_test, "candidate", ["--lib"])
        run(action, "recovery_candidate", recovery_test, "candidate", ["--lib"])
        run(
            action,
            "compiled_binary_preflight_rejection",
            preflight_test,
            "fail_closed",
            ["--test", "cli_read_only"],
        )


if platform_name == "linux":
    run(
        "create_copy",
        "executor_postcondition",
        "create_primitive_uses_only_the_recorded_temporary_and_verifies_its_postcondition",
        "candidate",
        ["--lib"],
    )
    run(
        "create_copy",
        "compiled_binary_candidate_success",
        "copy_create_applies_exact_source_bytes_and_commits_known_state",
        "candidate",
        ["--test", "cli_read_only"],
    )
    for phase in ["post_effect_aftermath", "recovery"]:
        run(
            "create_copy",
            phase,
            "recovery_keeps_matching_final_copy_create_uncertain_without_known_update",
            "candidate",
            ["--lib"],
        )
    run(
        "replace_copy",
        "native_no_replace_collision",
        "retained_parent_no_replace_rejects_an_existing_final_name",
        "candidate",
        ["--test", "native_copy_platform"],
    )
    run(
        "replace_copy",
        "native_no_replace_publication",
        "retained_parent_no_replace_publishes_exact_bytes_to_a_missing_final_name",
        "candidate",
        ["--test", "native_copy_platform"],
    )
    run_sequential_copy_candidates()
elif platform_name == "darwin":
    run(
        "create_copy",
        "native_collision_preservation",
        "retained_parent_no_replace_rejects_an_existing_final_name",
        "selected_and_enabled",
        ["--test", "native_copy_platform"],
    )
    run(
        "create_copy",
        "executor_postcondition",
        "create_primitive_uses_only_the_recorded_temporary_and_verifies_its_postcondition",
        "selected_and_enabled",
        ["--lib"],
    )
    run(
        "create_copy",
        "recovery",
        "recovery_keeps_matching_final_copy_create_uncertain_without_known_update",
        "selected_and_enabled",
        ["--lib"],
    )
    run(
        "create_copy",
        "compiled_cli_success",
        "copy_lifecycle_commands_render_typed_actions_and_apply_on_apfs",
        "selected_and_enabled",
        ["--test", "cli_read_only"],
    )
    run(
        "create_copy",
        "capability_rejection",
        "non_apfs_copy_publication_capability_is_a_read_only_preflight_rejection",
        "selected_and_enabled",
        ["--lib"],
    )
    run_sequential_copy_candidates()
else:
    run(
        "create_copy",
        "native_direct_create_candidate",
        "direct_exclusive_create_preserves_collision_and_publishes_exact_bytes",
        "candidate",
        ["--test", "native_copy_platform"],
    )
    run(
        "create_copy",
        "preflight_rejection",
        "copy_capability_preflight_rejection_creates_no_operation_or_target",
        "fail_closed",
        ["--test", "cli_read_only"],
    )
    for action in ["replace_copy", "remove_copy", "relocate_copy"]:
        run(
            action,
            "preflight_rejection",
            "copy_replace_relocate_and_remove_fail_preflight_without_mutating_target_or_state",
            "fail_closed",
            ["--test", "cli_read_only"],
        )
    for action, reason in [
        (
            "replace_copy",
            "the Windows retained-parent sequential candidate has no action-level native executor evidence",
        ),
        (
            "relocate_copy",
            "the Windows retained-parent relocation candidate has no action-level native executor evidence",
        ),
        (
            "link_to_copy_handoff",
            "requires a Windows file-symbolic-link fixture and action-level native executor evidence",
        ),
        (
            "copy_to_link_handoff",
            "requires a Windows file-symbolic-link fixture and action-level native executor evidence",
        ),
    ]:
        record_unrun(action, "native_executor_candidate", reason)

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
