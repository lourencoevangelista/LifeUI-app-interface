<#
  The PC half of the LifeUI agent: registers a daily Windows Task Scheduler job that runs the read-only
  agent_runner.py for your Obsidian vault (the cloud half runs in GitHub Actions, see .github/workflows/agent.yml).

  Run it yourself, once, in PowerShell (it changes Windows settings, so it is not run for you):
      powershell -ExecutionPolicy Bypass -File tools\install_pc_task.ps1 -Vault "C:\Path\To\Your\Vault" -Time 18:30
  Remove it again with:
      Unregister-ScheduledTask -TaskName "LifeUI agent (PC)" -Confirm:$false

  Before it can work, create the file  <repo>\.env.agent  (never committed) from tools\agent.env.example.
#>
param(
  [Parameter(Mandatory = $true)][string]$Vault,
  [string]$Time = "18:30",
  [ValidateSet("morning", "midday", "evening")][string]$Slot = "evening"
)

$ErrorActionPreference = "Stop"
$repo = Split-Path -Parent $PSScriptRoot
$python = Join-Path $repo ".venv\Scripts\python.exe"
if (-not (Test-Path $python)) { throw "No virtual environment at $python (create it and install tools\requirements.txt)." }
if (-not (Test-Path $Vault -PathType Container)) { throw "Vault folder not found: $Vault" }
$envFile = Join-Path $repo ".env.agent"
if (-not (Test-Path $envFile)) { throw "Missing $envFile - copy tools\agent.env.example to .env.agent and fill it in first." }

# The vault path goes into .env.agent (the runner reads it from there), not into the task definition.
$lines = Get-Content $envFile | Where-Object { $_ -notmatch '^\s*OBSIDIAN_VAULT=' }
$lines + "OBSIDIAN_VAULT=$Vault" | Set-Content -Path $envFile -Encoding UTF8

$action = New-ScheduledTaskAction -Execute $python -Argument "-I tools\agent_runner.py run --where pc --slot $Slot" -WorkingDirectory $repo
$trigger = New-ScheduledTaskTrigger -Daily -At $Time
# StartWhenAvailable: if the PC was off at that time, run once when it is next on.
$settings = New-ScheduledTaskSettingsSet -StartWhenAvailable -AllowStartIfOnBatteries -DontStopIfGoingOnBatteries -ExecutionTimeLimit (New-TimeSpan -Minutes 10)
Register-ScheduledTask -TaskName "LifeUI agent (PC)" -Action $action -Trigger $trigger -Settings $settings -Description "Read-only LifeUI agent: summarises recently changed Obsidian notes into the Agent feed." -Force | Out-Null
Write-Host "Registered 'LifeUI agent (PC)': daily at $Time, slot $Slot. Test it now with:  Start-ScheduledTask -TaskName 'LifeUI agent (PC)'"
