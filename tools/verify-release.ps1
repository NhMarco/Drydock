param(
    [string]$ExpectedVersion = ""
)

$ErrorActionPreference = "Stop"
$root = Split-Path -Parent $PSScriptRoot
$binary = Join-Path $root "target\release\Drydock.exe"

function Invoke-Cargo([string[]]$CargoArguments) {
    & cargo @CargoArguments
    if ($LASTEXITCODE -ne 0) {
        throw "cargo $($CargoArguments -join ' ') failed with exit code $LASTEXITCODE"
    }
}

function Invoke-DrydockCli([string]$Argument, [string]$Name) {
    $stdout = Join-Path $env:TEMP "drydock-$Name-$PID.txt"
    $stderr = Join-Path $env:TEMP "drydock-$Name-$PID.err.txt"
    try {
        $process = Start-Process -FilePath $binary -ArgumentList $Argument -Wait -PassThru `
            -WindowStyle Hidden -RedirectStandardOutput $stdout -RedirectStandardError $stderr
        [string]$errorText = ""
        if (Test-Path $stderr) {
            $errorText = Get-Content -Raw $stderr
        }
        if ($process.ExitCode -ne 0) {
            throw "Drydock $Argument failed with exit code $($process.ExitCode): $errorText"
        }
        if (-not [string]::IsNullOrWhiteSpace($errorText)) {
            throw "Drydock $Argument wrote to stderr: $errorText"
        }
        return (Get-Content -Raw $stdout).Trim()
    }
    finally {
        Remove-Item -LiteralPath $stdout, $stderr -Force -ErrorAction SilentlyContinue
    }
}

Push-Location $root
try {
    Invoke-Cargo -CargoArguments @("fmt", "--all", "--check")
    Invoke-Cargo -CargoArguments @("test", "--workspace", "--locked")
    Invoke-Cargo -CargoArguments @("clippy", "--workspace", "--all-targets", "--locked", "--", "-D", "warnings")
    Invoke-Cargo -CargoArguments @("build", "--locked", "--release", "-p", "drydock-desktop")

    $selfTest = Invoke-DrydockCli "--self-test" "self-test"
    if ($selfTest -notmatch "Drydock self-test passed") {
        throw "The packaged self-test did not report success: $selfTest"
    }
    $version = Invoke-DrydockCli "--version" "version"
    if ($version -notmatch '^\d+\.\d+\.\d+$') {
        throw "The packaged version is invalid: $version"
    }
    if ($ExpectedVersion -and $version -ne $ExpectedVersion) {
        throw "The packaged version '$version' does not match '$ExpectedVersion'"
    }

    $file = Get-Item -LiteralPath $binary
    $hash = (Get-FileHash -LiteralPath $binary -Algorithm SHA256).Hash
    [pscustomobject]@{
        Binary = $file.FullName
        Version = $version
        Bytes = $file.Length
        SHA256 = $hash
        SelfTest = $selfTest
    } | Format-List
}
finally {
    Pop-Location
}
