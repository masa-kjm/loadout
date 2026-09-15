# Windows Sandbox Runner

This runner executes the current v0.2 binary inside a disposable Windows Sandbox. It copies the shared v0.2 fixture into an isolated configuration, state, home, and local-store tree before running the selected CLI scenario.

## Requirements

- Windows 10/11 Pro or Enterprise
- Windows Sandbox enabled
- A release binary at `target\release\loadout.exe`
- PowerShell 5.1 or later

## Usage

From the repository root in PowerShell:

```powershell
.\tests\e2e\windows\sandbox\test.ps1 all
.\tests\e2e\windows\sandbox\test.ps1 -Scenario smoke -Build
.\tests\e2e\windows\sandbox\test.ps1 shell
```

`shell` prepares the isolated fixture environment and keeps the Sandbox open for manual commands. The generated `loadout.wsb` file and sandbox logs are ignored. The template and scripts are version-controlled; fixture inputs remain under `tests/fixtures/`.
